//! Consumed-D-Bus adapter: logind lifecycle signals → world system events.
//! Takes a delay inhibitor so TVs get their sleep/shutdown actions before
//! the system goes down, releasing it once the actions are dispatched.

use futures_util::StreamExt;
use tisvlay_world::{InEvent, SystemEvent, WorldClient};
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

    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;

    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Watch logind on `connection` for sleep/shutdown lifecycle and feed the
/// world. Runs until the bus connection dies.
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
                let Some(signal) = signal else { return Ok(()) };
                let Ok(args) = signal.args() else { continue };
                if args.start {
                    info!("system going to sleep");
                    world.send(InEvent::System { event: SystemEvent::Sleep, connector: None });
                    // Let the sleep proceed; retake on resume.
                    inhibitor.take();
                } else {
                    info!("system resumed");
                    world.send(InEvent::System { event: SystemEvent::Resume, connector: None });
                    inhibitor = take_inhibitor(&manager).await;
                }
            }
            signal = shutdown_stream.next() => {
                let Some(signal) = signal else { return Ok(()) };
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
                    inhibitor.take();
                }
            }
        }
    }
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
