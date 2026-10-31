//! Consumed-D-Bus adapter: logind lifecycle signals → world system events.
//! Takes a delay inhibitor so TVs get their sleep/shutdown actions before
//! the system goes down, releasing it once every connection has run the
//! commands those actions queued.

use std::future::Future;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_world::{DrainToken, InEvent, SleepMode, SleepRequest, SystemEvent, WorldClient};
use tokio::sync::mpsc;
use tracing::{info, warn};
use zbus::proxy;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    /// `(type, usec)` of the scheduled shutdown — `reboot` / `kexec` /
    /// `poweroff`. Read when PrepareForShutdown fires to pick the
    /// profile event.
    #[zbus(property)]
    fn scheduled_shutdown(&self) -> zbus::Result<(String, u64)>;

    /// How long logind waits on a delay inhibitor before proceeding
    /// anyway (`InhibitDelayMaxSec=`), in microseconds.
    #[zbus(property, name = "InhibitDelayMaxUSec")]
    fn inhibit_delay_max_usec(&self) -> zbus::Result<u64>;

    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;

    /// Suspend to RAM. `interactive: false`: never prompt, the polkit
    /// rule decides.
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;

    /// The host's configured sleep (`SleepOperation=`); systemd 256 or
    /// newer. `flags: 0` behaves like the unflagged methods.
    fn sleep(&self, flags: u64) -> zbus::Result<()>;

    /// Whether this caller may suspend: `yes`, `no`, `challenge` (it
    /// would prompt), `na`, or an inhibitor answer.
    fn can_suspend(&self) -> zbus::Result<String>;

    /// [`Self::can_suspend`] for [`Self::sleep`]; systemd 256 or newer.
    fn can_sleep(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Pause between a lost watch and the next connection attempt. Short,
/// since a suspend that lands in the gap leaves the TVs on.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// logind's default `InhibitDelayMaxSec=`, the bound when its property
/// can't be read.
const DEFAULT_INHIBIT_DELAY_MAX: Duration = Duration::from_secs(5);

/// Supervised entry: watch logind over a fresh system-bus connection,
/// reconnecting after [`RECONNECT_DELAY`] whenever the watch ends.
pub(crate) fn spawn(world: WorldClient) {
    tokio::spawn(supervise_logind(
        world,
        zbus::Connection::system,
        RECONNECT_DELAY,
    ));
}

/// The daemon's logind supervision loop, with the connection source and
/// the delay as parameters so a test can drive it over a private bus.
///
/// A bus can drop a peer: dbus-broker evicts one whose user is over its
/// quota, and the broker itself can restart. Unsupervised, a dropped
/// connection silently ended sleep, resume and shutdown handling for the
/// daemon's lifetime. A loss warns once; a bus that stays down doesn't
/// repeat it, but a watch that ran for a while was working, so its loss
/// warns again.
pub async fn supervise_logind<C, F>(world: WorldClient, connect: C, delay: Duration)
where
    C: Fn() -> F,
    F: Future<Output = zbus::Result<zbus::Connection>>,
{
    let mut warned = false;
    loop {
        let started = tokio::time::Instant::now();
        let outcome = match connect().await {
            Ok(connection) => watch_logind(connection, world.clone()).await,
            Err(e) => Err(e.into()),
        };
        if started.elapsed() > Duration::from_secs(60) {
            warned = false;
        }
        if let Err(e) = outcome
            && !std::mem::replace(&mut warned, true)
        {
            warn!(error = %format!("{e:#}"), retry_in = ?delay, "logind watcher lost; reconnecting");
        }
        tokio::time::sleep(delay).await;
    }
}

/// Watch logind on `connection` for sleep/shutdown lifecycle and feed the
/// world. Runs until the bus connection dies, which is an error: the
/// supervisor reconnects.
pub async fn watch_logind(connection: zbus::Connection, world: WorldClient) -> anyhow::Result<()> {
    let manager = Login1ManagerProxy::new(&connection).await?;
    // Best-effort delay inhibitor: without it the box may sleep before the
    // TVs get their actions. A fake logind without Inhibit is fine.
    let mut inhibitor = take_inhibitor(&manager).await;
    let mut sleep_stream = manager.receive_prepare_for_sleep().await?;
    let mut shutdown_stream = manager.receive_prepare_for_shutdown().await?;
    info!("logind watcher started");
    loop {
        tokio::select! {
            signal = sleep_stream.next() => {
                let Some(signal) = signal else { anyhow::bail!("logind's sleep signals ended") };
                let Ok(args) = signal.args() else { continue };
                if args.start {
                    info!("system going to sleep");
                    world.send(InEvent::System { event: SystemEvent::Sleep, connector: None });
                    // Let the sleep proceed once the TVs' commands ran;
                    // retake on resume.
                    release_after_drain(&world, &manager, &mut inhibitor).await;
                } else {
                    info!("system resumed");
                    world.send(InEvent::System { event: SystemEvent::Resume, connector: None });
                    inhibitor = take_inhibitor(&manager).await;
                }
            }
            signal = shutdown_stream.next() => {
                let Some(signal) = signal else { anyhow::bail!("logind's shutdown signals ended") };
                let Ok(args) = signal.args() else { continue };
                if args.start {
                    // Reboot vs poweroff decides the profile event: the
                    // stock mapping keeps the TV on across a quick
                    // reboot (on_reboot: none) but powers it off for a
                    // real shutdown. Property read failure assumes
                    // poweroff, the conservative action.
                    let event = match manager.scheduled_shutdown().await {
                        Ok((kind, _)) if kind == "reboot" || kind == "kexec" => {
                            SystemEvent::Reboot
                        }
                        _ => SystemEvent::Shutdown,
                    };
                    info!(?event, "system shutting down");
                    world.send(InEvent::System { event, connector: None });
                    release_after_drain(&world, &manager, &mut inhibitor).await;
                }
            }
        }
    }
}

/// Release the delay inhibitor once every live connection has run the
/// commands queued before this call (the event just sent), or at logind's
/// `InhibitDelayMaxUSec`, after which logind proceeds without us anyway.
async fn release_after_drain(
    world: &WorldClient,
    manager: &Login1ManagerProxy<'_>,
    inhibitor: &mut Option<zbus::zvariant::OwnedFd>,
) {
    if inhibitor.is_some() {
        let (token, drained) = DrainToken::pair();
        world.send(InEvent::Drain(token));
        let bound = manager
            .inhibit_delay_max_usec()
            .await
            .map_or(DEFAULT_INHIBIT_DELAY_MAX, Duration::from_micros);
        if tokio::time::timeout(bound, drained.wait()).await.is_err() {
            warn!(
                ?bound,
                "TV commands still running at logind's inhibit delay, releasing"
            );
        }
    }
    inhibitor.take();
}

async fn take_inhibitor(manager: &Login1ManagerProxy<'_>) -> Option<zbus::zvariant::OwnedFd> {
    match manager
        .inhibit(
            "sleep:shutdown",
            "tisvlay",
            "dispatching TV power actions",
            "delay",
        )
        .await
    {
        Ok(fd) => Some(fd),
        Err(e) => {
            warn!(%e, "could not take logind inhibitor (continuing without)");
            None
        }
    }
}

/// Spawn the sleeper: each of the system profile's sleep requests (a TV
/// powered off by itself) puts the machine to sleep through logind on a
/// fresh system-bus connection. A refusal goes back to the world, which
/// publishes it as `event/dispatch-failed`.
pub(crate) fn spawn_sleeper(requests: mpsc::UnboundedReceiver<SleepRequest>, world: WorldClient) {
    tokio::spawn(async move {
        let mut requests = requests;
        while let Some(request) = requests.recv().await {
            match zbus::Connection::system().await {
                Ok(connection) => sleep_for(&connection, request, &world).await,
                Err(e) => refused(&world, request, &e),
            }
        }
    });
}

/// The sleeper over an injected `connection`, the entry integration
/// tests drive against a private bus. Runs until the requests end.
pub async fn run_sleeper(
    connection: zbus::Connection,
    mut requests: mpsc::UnboundedReceiver<SleepRequest>,
    world: WorldClient,
) {
    while let Some(request) = requests.recv().await {
        sleep_for(&connection, request, &world).await;
    }
}

async fn sleep_for(connection: &zbus::Connection, request: SleepRequest, world: &WorldClient) {
    let outcome = match Login1ManagerProxy::new(connection).await {
        Ok(manager) => match request.mode {
            SleepMode::Suspend => manager.suspend(false).await,
            SleepMode::Sleep => manager.sleep(0).await,
        },
        Err(e) => Err(e),
    };
    match outcome {
        Ok(()) => info!(tv = %request.tv, mode = request.mode.as_str(), "asked logind to sleep"),
        Err(e) => refused(world, request, &e),
    }
}

/// Report a sleep logind wouldn't take: polkit refused it, an inhibitor
/// blocked it, or the host lacks the method (`Sleep` before systemd 256).
fn refused(world: &WorldClient, request: SleepRequest, error: &zbus::Error) {
    warn!(tv = %request.tv, mode = request.mode.as_str(), %error, "logind refused the sleep");
    world.send(InEvent::SleepRefused {
        tv: request.tv,
        mode: request.mode,
        message: error.to_string(),
    });
}

/// Which sleep methods logind grants this daemon without a prompt, for
/// the info reply: only `yes` counts, since the daemon has no agent to
/// answer a `challenge`. `None` without a system bus or logind.
pub(crate) async fn sleep_support() -> Option<tisvlay_protocol::rpc::SleepSupport> {
    let connection = zbus::Connection::system().await.ok()?;
    let manager = Login1ManagerProxy::new(&connection).await.ok()?;
    let granted = |answer: zbus::Result<String>| answer.is_ok_and(|a| a == "yes");
    Some(tisvlay_protocol::rpc::SleepSupport {
        suspend: granted(manager.can_suspend().await),
        sleep: granted(manager.can_sleep().await),
    })
}
