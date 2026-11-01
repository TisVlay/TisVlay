//! tisvlay-tray: the root ksni tray reshaped for zenoh. The menu renders
//! from the SDK clients' mirrors of the daemon (plus the session
//! service's user keys when its socket exists); every click is an rpc.

pub mod notify;
pub mod service;
pub mod state;
pub mod tray;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use ksni::TrayMethods;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{Unary, tv, user_profile};
use tisvlay_protocol::payload::{self, PairingPhase};
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_runtime::client::{Coalescer, ReconcilerMap};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;
use zenoh::Session;

use tray::{ScrollTarget, TrayCommand, TvTray};

#[derive(Clone)]
enum TrayHandle {
    Desktop(ksni::Handle<TvTray>),
    #[cfg(test)]
    Headless(Arc<tokio::sync::Mutex<TvTray>>),
}

impl TrayHandle {
    async fn update<R>(&self, update: impl FnOnce(&mut TvTray) -> R) -> Option<R> {
        match self {
            Self::Desktop(handle) => handle.update(update).await,
            #[cfg(test)]
            Self::Headless(tray) => Some(update(&mut *tray.lock().await)),
        }
    }
}

/// Optimistic per-TV intent for the scroll targets — the root stack's
/// reconciler pattern: scroll math bases on what the user last asked
/// for rather than the daemon cache bouncing during a TV-side ramp, and
/// the observed echo releases the gate.
#[derive(Clone)]
struct Reconcilers {
    volume: Arc<ReconcilerMap<Uuid, u8>>,
    backlight: Arc<ReconcilerMap<Uuid, u8>>,
}

impl Reconcilers {
    fn new() -> Self {
        Self {
            volume: Arc::new(ReconcilerMap::new()),
            backlight: Arc::new(ReconcilerMap::new()),
        }
    }
}

impl std::fmt::Debug for Reconcilers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reconcilers").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct TrayOptions {
    pub system_socket: PathBuf,
    /// The session service's socket; absent or unreachable = no user
    /// profiles in the menu this run.
    pub user_socket: PathBuf,
    pub site: String,
    /// The tray's own file — notification settings (tray.toml).
    pub config: PathBuf,
}

/// Wire the tray: zenoh sessions, ksni item, the SDK clients, command
/// processor. Runs until ctrl-c.
pub async fn run(options: TrayOptions) -> Result<()> {
    let system = zenoh::open(
        bootstrap::client_config_resilient(&options.system_socket)
            .context("system session config")?,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "connect to system socket {}: {e}",
            options.system_socket.display()
        )
    })?;
    // Background-retry link: the session service may start (or restart)
    // any time; user keys hydrate whenever it appears.
    let user = match bootstrap::client_config_resilient(&options.user_socket) {
        Ok(config) => match zenoh::open(config).await {
            Ok(session) => Some(session),
            Err(e) => {
                warn!(%e, "user socket session failed; user profiles hidden");
                None
            }
        },
        Err(e) => {
            warn!(%e, "user socket config failed");
            None
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let open_gui_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Notifications: category-gated toasts behind one emit worker; the
    // switches persist to tray.toml and are served on the user socket.
    let notifier =
        notify::Notifier::new(notify::load_settings(&options.config).unwrap_or_default());
    // The settings service SERVES (queryable + token + cached pub), and
    // leaf peers are consume-only in this topology — so it runs on a
    // supervised client-mode link to the user hub: fail-fast opens in a
    // retry loop, re-serving after every hub restart. It's independent of
    // the tray icon, so it starts first: the GUI reaches the tray over the
    // user socket even where the desktop has no StatusNotifier host.
    tokio::spawn({
        let user_socket = options.user_socket.clone();
        let site = options.site.clone();
        let config = options.config.clone();
        let notifier = notifier.clone();
        async move {
            loop {
                match bootstrap::client_config(&user_socket) {
                    Ok(link_config) => match zenoh::open(link_config).await {
                        Ok(link) => {
                            if let Err(e) = notify::serve_settings(
                                link,
                                site.clone(),
                                config.clone(),
                                notifier.clone(),
                            )
                            .await
                            {
                                warn!(%e, "tray settings service exited");
                            }
                        }
                        Err(e) => debug!(%e, "user hub not up yet; settings service waiting"),
                    },
                    Err(e) => {
                        warn!(%e, "user socket config failed; settings service disabled");
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    });

    // Tray icon. `assume_sni_available(true)` keeps a missing StatusNotifier
    // host — vanilla GNOME with no AppIndicator extension — from failing the
    // spawn: the handle stays live so the menu and notifications keep
    // working, and the icon registers if a host appears later (extension
    // enabled, or a desktop like KDE that ships one). Only a genuine D-Bus
    // failure errors here; we log and stay up on the settings service above
    // rather than let `Restart=on-failure` crash-loop the process.
    let handle = match TvTray::new(cmd_tx, Arc::clone(&open_gui_pending))
        .assume_sni_available(true)
        .spawn()
        .await
    {
        Ok(handle) => TrayHandle::Desktop(handle),
        Err(e) => {
            warn!(%e, "tray icon unavailable; running headless (settings service only)");
            tokio::signal::ctrl_c().await.context("wait for shutdown")?;
            return Ok(());
        }
    };
    spawn_pairing_toasts(&system, &options.site, handle.clone(), notifier.clone()).await?;
    spawn_power_cycle_toasts(&system, &options.site, handle.clone(), notifier.clone()).await?;

    let reconcilers = Reconcilers::new();
    // The SDK's clients over the tray's own links, one per socket: they
    // follow the daemon's and the session service's lifetimes.
    let daemon = tisvlay::DaemonClient::with_session(system.clone(), &options.site)
        .await
        .map_err(|e| anyhow!("mirror the daemon: {e}"))?;
    let session = match &user {
        Some(user) => {
            match tisvlay::SessionClient::with_session(user.clone(), &options.site).await {
                Ok(session) => Some(session),
                Err(e) => {
                    warn!(%e, "couldn't mirror the session service; user profiles hidden");
                    None
                }
            }
        }
        None => None,
    };
    tokio::spawn(follow_services(
        daemon,
        session,
        handle.clone(),
        reconcilers.clone(),
        notifier.clone(),
    ));
    // Matching-gated rpc callers, one per socket — menu clicks and
    // scroll writes ride them so route propagation is waited out
    // instead of dropping the action.
    let caller = Arc::new(Caller::new(system.clone(), options.site.clone()));
    let user_caller = user
        .clone()
        .map(|session| Arc::new(Caller::new(session, options.site.clone())));
    // One coalescer per scroll target: rapid ticks fold into one rpc per
    // TV, based on reconciled intent, with at most one write in flight.
    let volume_scroll = scroll_coalescer(
        Arc::clone(&caller),
        handle.clone(),
        reconcilers.clone(),
        ScrollTarget::Volume,
    );
    let backlight_scroll = scroll_coalescer(
        Arc::clone(&caller),
        handle.clone(),
        reconcilers.clone(),
        ScrollTarget::Backlight,
    );
    tokio::spawn(process_commands(
        Arc::clone(&caller),
        user_caller,
        cmd_rx,
        handle.clone(),
        volume_scroll,
        backlight_scroll,
        open_gui_pending,
    ));

    tokio::signal::ctrl_c().await.context("wait for shutdown")?;
    info!("shutting down");
    Ok(())
}

/// Render the menu from the SDK clients' mirrors, rebuilt on each of their
/// events: the clients follow the daemon's and the session service's
/// lifetimes (subscribing per token lifetime, clearing on loss), so the
/// menu does too. Each rebuild feeds the TVs' levels to the scroll
/// reconcilers, and toasts connection changes and the daemon going
/// offline or coming back.
async fn follow_services(
    daemon: tisvlay::DaemonClient,
    session: Option<tisvlay::SessionClient>,
    handle: TrayHandle,
    reconcilers: Reconcilers,
    notifier: notify::Notifier,
) {
    let mut watcher =
        tisvlay::StateWatcher::new(&daemon, session.as_ref(), None, Default::default());
    // The daemon's presence before the watcher's first event isn't known
    // yet, so its first reading is no transition (and an offline toast at
    // every tray start would be noise).
    let mut last_alive: Option<bool> = None;
    let mut started = false;
    loop {
        let view = snapshot(&daemon, session.as_ref()).await;
        for (id, tv) in &view.tvs {
            // Echoed levels release the scroll-intent gates; an unchanged
            // one is a no-op.
            if let Some(level) = tv.volume {
                let _ = reconcilers.volume.observe(id, level);
            }
            if let Some(level) = tv.backlight {
                let _ = reconcilers.backlight.observe(id, level);
            }
        }
        let alive = view.daemon_alive;
        let changes = handle
            .update(move |tray| {
                let changes = view.connection_changes(&tray.view);
                tray.view = view;
                changes
            })
            .await
            .unwrap_or_default();
        for (name, connected) in changes {
            let (summary, icon) = if connected {
                (format!("{name} connected"), "video-display")
            } else {
                (format!("{name} disconnected"), "video-display-symbolic")
            };
            notifier.notify(notify::Category::TvConnection, &summary, "", icon);
        }
        if let Some(was) = last_alive
            && was != alive
        {
            let (summary, icon) = if alive {
                ("TisVlay daemon is back online", "video-display")
            } else {
                ("TisVlay daemon went offline", "dialog-warning")
            };
            notifier.notify(notify::Category::DaemonOffline, summary, "", icon);
        }
        if started {
            last_alive = Some(alive);
        }
        started = true;
        if watcher.next().await.is_none() {
            return;
        }
    }
}

/// The menu's view of what the clients mirror right now.
async fn snapshot(
    daemon: &tisvlay::DaemonClient,
    session: Option<&tisvlay::SessionClient>,
) -> state::View {
    let tvs = daemon.list_tvs().await.unwrap_or_default();
    let system_profiles = daemon.list_system_profile_bodies().await;
    let mut user_profiles = std::collections::HashMap::new();
    let mut assignments = std::collections::HashMap::new();
    if let Some(session) = session {
        user_profiles = session.list_user_profile_bodies().await;
        for tv in &tvs {
            if let Some(profile) = session.get_tv_user_profile(tv.uuid).await {
                assignments.insert(tv.uuid, profile);
            }
        }
    }
    let mut view = state::View::new(&tvs, &system_profiles, &user_profiles, |tv| {
        assignments.get(&tv).copied()
    });
    view.daemon_alive = daemon.is_available().await;
    view.daemon_hydrated = daemon.is_hydrated().await;
    view
}

/// Execute menu commands as rpcs. Scroll bursts hand off to the
/// per-target coalescers without awaiting, so a slow set-volume can't
/// block other commands behind it.
async fn process_commands(
    caller: Arc<Caller>,
    user_caller: Option<Arc<Caller>>,
    mut rx: mpsc::UnboundedReceiver<TrayCommand>,
    handle: TrayHandle,
    volume_scroll: Coalescer<Uuid, i32>,
    backlight_scroll: Coalescer<Uuid, i32>,
    open_gui_pending: Arc<std::sync::atomic::AtomicBool>,
) {
    while let Some(command) = rx.recv().await {
        if matches!(command, TrayCommand::OpenGui) {
            // The menu callback check-and-set the pending flag before
            // sending; clear it once the Activate returns so the next
            // click can enqueue again.
            if let Err(e) = activate_gui().await {
                warn!(error = %e, "open GUI failed");
            }
            open_gui_pending.store(false, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        if let TrayCommand::ScrollBy(id, target, steps) = command {
            if steps != 0 {
                match target {
                    ScrollTarget::Volume => volume_scroll.fire(id, steps),
                    ScrollTarget::Backlight => backlight_scroll.fire(id, steps),
                }
            }
            continue;
        }
        execute(&caller, user_caller.as_deref(), command, &handle).await;
    }
}

/// Build the sum-merge coalescer for one scroll target. The apply reads
/// the reconciled intent (the view's value only as first fallback), clamps,
/// and dispatches the new intent *before* awaiting the rpc so the gate
/// is in place before any subscription pushes arrive.
fn scroll_coalescer(
    caller: Arc<Caller>,
    handle: TrayHandle,
    reconcilers: Reconcilers,
    target: ScrollTarget,
) -> Coalescer<Uuid, i32> {
    Coalescer::new(
        |old: i32, new: i32| old + new,
        move |id: Uuid, steps: i32| {
            let caller = Arc::clone(&caller);
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = handle
                    .update(|tray| {
                        let tv = tray.view.tvs.get(&id)?;
                        match target {
                            ScrollTarget::Volume => tv.volume,
                            ScrollTarget::Backlight => tv.backlight,
                        }
                    })
                    .await
                    .flatten();
                let Some(fallback) = fallback else {
                    return;
                };
                let map = match target {
                    ScrollTarget::Volume => &reconcilers.volume,
                    ScrollTarget::Backlight => &reconcilers.backlight,
                };
                let current = map.current(&id, fallback);
                let level = (i32::from(current) + steps).clamp(0, 100) as u8;
                if level == current {
                    return;
                }
                map.dispatch(&id, level);
                match target {
                    ScrollTarget::Volume => {
                        call::<tv::SetVolume>(&caller, &id, &rpc::SetVolume { level }).await;
                    }
                    ScrollTarget::Backlight => {
                        call::<tv::SetBacklight>(&caller, &id, &rpc::SetBacklight { level }).await;
                    }
                }
            }
        },
    )
}

async fn execute(
    caller: &Caller,
    user_caller: Option<&Caller>,
    command: TrayCommand,
    handle: &TrayHandle,
) {
    debug!(?command, "tray command");
    match command {
        TrayCommand::PowerOn(id) => {
            call::<tv::PowerOn>(caller, &id, &rpc::PowerOn::default()).await;
        }
        TrayCommand::PowerOff(id) => {
            call::<tv::PowerOff>(caller, &id, &()).await;
        }
        TrayCommand::ScreenOn(id) => {
            call::<tv::SetScreen>(caller, &id, &rpc::SetScreen::manual(true)).await;
        }
        TrayCommand::ScreenOff(id) => {
            call::<tv::SetScreen>(caller, &id, &rpc::SetScreen::manual(false)).await;
        }
        TrayCommand::SwitchInput(id, input) => {
            call::<tv::SwitchInput>(caller, &id, &rpc::SwitchInput { input }).await;
        }
        TrayCommand::SetSystemProfile(id, profile) => {
            call::<tv::SetSystemProfile>(caller, &id, &rpc::SetSystemProfile { profile }).await;
        }
        TrayCommand::SetUserProfile(tv, profile) => {
            let Some(user_caller) = user_caller else {
                warn!("no session service; user profile change dropped");
                return;
            };
            call::<user_profile::Assign>(user_caller, &(), &rpc::UserAssign { tv, profile }).await;
        }
        TrayCommand::ToggleMute(id) => {
            let muted = handle
                .update(|tray| tray.view.tvs.get(&id).and_then(|tv| tv.muted))
                .await
                .flatten()
                .unwrap_or(false);
            call::<tv::SetMuted>(caller, &id, &rpc::SetMuted { muted: !muted }).await;
        }
        TrayCommand::SetManaged(id, managed) => {
            call::<tv::SetManaged>(caller, &id, &rpc::SetManaged { managed }).await;
        }
        // Handled by the coalescers in process_commands.
        TrayCommand::ScrollBy(..) => {}
        // Handled in process_commands (needs the pending flag).
        TrayCommand::OpenGui => {}
        #[cfg(debug_assertions)]
        #[expect(
            clippy::exit,
            reason = "debug-only Quit ends the tray process immediately"
        )]
        TrayCommand::Quit => std::process::exit(0),
    }
}

/// Invoke `org.freedesktop.Application.Activate` on the running GUI
/// (or auto-spawn one via D-Bus session activation when the `.service`
/// file is installed). `platform_data` stays empty — the tray is
/// surfaceless, so it has no useful `xdg-activation-v1` token to pass;
/// the GUI handles the raise/attention UX itself.
async fn activate_gui() -> Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value as ZValue;

    let conn = zbus::Connection::session()
        .await
        .context("connect to session bus")?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Gui",
        "/org/tisvlay/Gui",
        "org.freedesktop.Application",
    )
    .await
    .context("build org.freedesktop.Application proxy")?;
    let platform_data: HashMap<String, ZValue<'_>> = HashMap::new();
    let _: () = proxy
        .call("Activate", &(platform_data,))
        .await
        .context("call org.freedesktop.Application.Activate")?;
    Ok(())
}

/// One rpc round-trip; failures land in the log, not the UI — the view
/// shows the truth either way. Matching-gated single shot: a click
/// during route propagation waits it out instead of vanishing.
async fn call<R: Unary>(caller: &Caller, target: &R::Target, request: &R::Request) {
    if let Err(error) = caller.call::<R>(target, request).await {
        warn!(rpc = R::NAME, %error, "rpc failed");
    }
}

/// Default session-service socket, shared shape with `tisvlay-session`.
pub fn default_user_socket() -> PathBuf {
    bootstrap::default_session_socket_path()
}

/// Terminal pairing phases become toasts — non-terminal progress is the
/// wizard's job to surface, not the tray's (root stance).
async fn spawn_pairing_toasts(
    system: &Session,
    site: &str,
    handle: TrayHandle,
    notifier: notify::Notifier,
) -> Result<()> {
    // Plain subscriber: the deep pairing cache would replay finished
    // sessions as fresh toasts on every tray start.
    let sub = system
        .declare_subscriber(keys::pairing_progress_wild(site))
        .await
        .map_err(|e| anyhow!("subscribe pairing progress: {e}"))?;
    let site = site.to_string();
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let Some(tv) = keys::parse_pairing_progress(&site, sample.key_expr().as_str()) else {
                continue;
            };
            let Ok(progress) =
                serde_json::from_slice::<payload::PairingProgress>(&sample.payload().to_bytes())
            else {
                continue;
            };
            let name = handle
                .update(|tray| tray.view.tvs.get(&tv).map(|view| view.name.clone()))
                .await
                .flatten()
                .unwrap_or_else(|| "TV".into());
            match progress.phase {
                PairingPhase::Complete => notifier.notify(
                    notify::Category::TvPairing,
                    &format!("{name} paired"),
                    &progress.message,
                    "security-high",
                ),
                PairingPhase::Failed => notifier.notify(
                    notify::Category::TvPairing,
                    &format!("Pairing {name} failed"),
                    &progress.message,
                    "dialog-error",
                ),
                PairingPhase::AwaitingTv | PairingPhase::AwaitingInput => {}
            }
        }
    });
    Ok(())
}

/// Wake feedback: `wol_sent` → waking toast, then connected/failed.
async fn spawn_power_cycle_toasts(
    system: &Session,
    site: &str,
    handle: TrayHandle,
    notifier: notify::Notifier,
) -> Result<()> {
    let sub = system
        .declare_subscriber(format!("tisvlay/{site}/tv/*/progress/power"))
        .await
        .map_err(|e| anyhow!("subscribe power progress: {e}"))?;
    let site = site.to_string();
    tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let Some((tv, _)) = keys::parse_tv_path(&site, sample.key_expr().as_str()) else {
                continue;
            };
            let Ok(progress) =
                serde_json::from_slice::<payload::PowerProgress>(&sample.payload().to_bytes())
            else {
                continue;
            };
            let name = handle
                .update(|tray| tray.view.tvs.get(&tv).map(|view| view.name.clone()))
                .await
                .flatten()
                .unwrap_or_else(|| "TV".into());
            match progress.phase {
                payload::PowerPhase::WolSent => notifier.notify(
                    notify::Category::TvWake,
                    &format!("Waking {name}…"),
                    "",
                    "system-run",
                ),
                payload::PowerPhase::Connected => notifier.notify(
                    notify::Category::TvWake,
                    &format!("{name} is awake"),
                    "",
                    "video-display",
                ),
                payload::PowerPhase::Failed => notifier.notify(
                    notify::Category::TvWake,
                    &format!("Waking {name} failed"),
                    "",
                    "dialog-error",
                ),
                payload::PowerPhase::Connecting
                | payload::PowerPhase::AwaitingNetwork
                | payload::PowerPhase::AwaitingDevice => {}
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;

    async fn wait_for_tray(handle: &TrayHandle, ready: impl Fn(&TvTray) -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                if handle.update(|tray| ready(tray)).await.unwrap() {
                    return;
                }
                tick.tick().await;
            }
        })
        .await
        .expect("the tray's view did not converge");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_and_commands_round_trip_without_a_desktop() {
        let directory = tisvlay_test::socket_tempdir();
        let socket = directory.path().join("tray.sock");
        let (connector, discovery, _mock) = tisvlay_mock::new_test_parts();
        let stack = tisvlay_daemon::serve(
            &socket,
            "test",
            vec![
                tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                    .with_discovery(Arc::new(discovery)),
            ],
            None,
        )
        .await
        .unwrap();
        let system = zenoh::open(bootstrap::client_config(&socket).unwrap())
            .await
            .unwrap();
        let caller = Arc::new(Caller::new(system.clone(), "test"));
        let daemon = tisvlay::DaemonClient::connect(&socket, "test")
            .await
            .unwrap();
        let mut watcher = tisvlay::StateWatcher::new(&daemon, None, None, Default::default());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !daemon.is_available().await {
                watcher.next().await.unwrap();
            }
        })
        .await
        .unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        let command_sender = commands.clone();
        let pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = TrayHandle::Headless(Arc::new(tokio::sync::Mutex::new(TvTray::new(
            commands,
            pending.clone(),
        ))));
        let (notifier, mut notifications) = notify::Notifier::capturing(Default::default());
        let reconcilers = Reconcilers::new();
        // The session service starts later on this socket; its client
        // follows it whenever it appears.
        let user_socket = directory.path().join("user.sock");
        let session = tisvlay::SessionClient::connect(&user_socket, "test")
            .await
            .unwrap();
        tokio::spawn(follow_services(
            daemon.clone(),
            Some(session.clone()),
            handle.clone(),
            reconcilers.clone(),
            notifier.clone(),
        ));
        spawn_pairing_toasts(&system, "test", handle.clone(), notifier.clone())
            .await
            .unwrap();
        spawn_power_cycle_toasts(&system, "test", handle.clone(), notifier)
            .await
            .unwrap();
        let tv = daemon
            .pair_tv(
                "Tray TV",
                "mock",
                serde_json::from_value(
                    serde_json::json!({"address":"mock://tray", "pairing":"accept"}),
                )
                .unwrap(),
                false,
            )
            .await
            .unwrap()
            .id;
        wait_for_tray(&handle, |tray| {
            tray.view.daemon_alive && tray.view.tvs.get(&tv).is_some_and(|view| view.connected)
        })
        .await;
        execute(&caller, None, TrayCommand::ToggleMute(tv), &handle).await;
        wait_for_tray(&handle, |tray| tray.view.tvs[&tv].muted == Some(true)).await;
        execute(&caller, None, TrayCommand::ToggleMute(tv), &handle).await;
        wait_for_tray(&handle, |tray| tray.view.tvs[&tv].muted == Some(false)).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(toast) = notifications.recv().await {
                if toast.summary.ends_with(" paired") {
                    return;
                }
            }
            panic!("notification capture closed");
        })
        .await
        .unwrap();
        execute(&caller, None, TrayCommand::ScreenOff(tv), &handle).await;
        wait_for_tray(&handle, |tray| !tray.view.tvs[&tv].is_screen_on).await;
        execute(&caller, None, TrayCommand::ScreenOn(tv), &handle).await;
        wait_for_tray(&handle, |tray| tray.view.tvs[&tv].is_screen_on).await;
        execute(
            &caller,
            None,
            TrayCommand::SwitchInput(tv, "HDMI_2".into()),
            &handle,
        )
        .await;
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].current_input.as_deref() == Some("HDMI_2")
        })
        .await;
        execute(&caller, None, TrayCommand::SetManaged(tv, true), &handle).await;
        wait_for_tray(&handle, |tray| tray.view.tvs[&tv].managed).await;
        execute(&caller, None, TrayCommand::SetManaged(tv, false), &handle).await;
        wait_for_tray(&handle, |tray| !tray.view.tvs[&tv].managed).await;
        let profile = daemon.create_system_profile("Tray work").await.unwrap();
        execute(
            &caller,
            None,
            TrayCommand::SetSystemProfile(tv, Some(profile)),
            &handle,
        )
        .await;
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].system_profile == Some(profile)
                && tray.view.system_profiles.contains_key(&profile)
        })
        .await;
        execute(&caller, None, TrayCommand::PowerOff(tv), &handle).await;
        wait_for_tray(&handle, |tray| !tray.view.tvs[&tv].is_on).await;
        execute(&caller, None, TrayCommand::PowerOn(tv), &handle).await;
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].connected && tray.view.tvs[&tv].is_on
        })
        .await;
        execute(
            &caller,
            None,
            TrayCommand::SetUserProfile(tv, None),
            &handle,
        )
        .await;

        let session_stack = tisvlay_session::serve(tisvlay_session::SessionOptions {
            system_socket: socket.clone(),
            user_socket: user_socket.clone(),
            site: "test".into(),
            store: None,
            sunshine: tisvlay_session::sunshine::StaticProbe::absent(),
        })
        .await
        .unwrap();
        let user = zenoh::open(bootstrap::client_config(&user_socket).unwrap())
            .await
            .unwrap();
        let user_caller = Arc::new(Caller::new(user.clone(), "test"));
        let mut session_events =
            tisvlay::StateWatcher::new(&daemon, Some(&session), None, Default::default());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session.is_available().await {
                session_events.next().await.unwrap();
            }
        })
        .await
        .unwrap();
        let user_profile = session.create_user_profile("Tray personal").await.unwrap();
        execute(
            &caller,
            Some(&user_caller),
            TrayCommand::SetUserProfile(tv, Some(user_profile)),
            &handle,
        )
        .await;
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].user_profile == Some(user_profile)
                && tray.view.user_profiles.contains_key(&user_profile)
        })
        .await;
        let volume = scroll_coalescer(
            caller.clone(),
            handle.clone(),
            reconcilers.clone(),
            ScrollTarget::Volume,
        );
        let backlight = scroll_coalescer(
            caller.clone(),
            handle.clone(),
            reconcilers,
            ScrollTarget::Backlight,
        );
        let processor = tokio::spawn(process_commands(
            caller.clone(),
            Some(user_caller),
            receiver,
            handle.clone(),
            volume,
            backlight,
            pending,
        ));
        for (target, steps) in [(ScrollTarget::Volume, 500), (ScrollTarget::Backlight, 500)] {
            command_sender
                .send(TrayCommand::ScrollBy(tv, target, steps))
                .unwrap();
        }
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].volume == Some(100) && tray.view.tvs[&tv].backlight == Some(100)
        })
        .await;
        for (target, steps) in [
            (ScrollTarget::Volume, -500),
            (ScrollTarget::Backlight, -500),
        ] {
            command_sender
                .send(TrayCommand::ScrollBy(tv, target, steps))
                .unwrap();
        }
        wait_for_tray(&handle, |tray| {
            tray.view.tvs[&tv].volume == Some(0) && tray.view.tvs[&tv].backlight == Some(0)
        })
        .await;
        command_sender.send(TrayCommand::ToggleMute(tv)).unwrap();
        wait_for_tray(&handle, |tray| tray.view.tvs[&tv].muted == Some(true)).await;
        processor.abort();
        assert!(processor.await.unwrap_err().is_cancelled());
        drop(session_stack);
        stack.run_until(std::future::ready(())).await.unwrap();
        wait_for_tray(&handle, |tray| !tray.view.daemon_alive).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(toast) = notifications.recv().await {
                if toast.summary == "TisVlay daemon went offline" {
                    return;
                }
            }
            panic!("notification capture closed");
        })
        .await
        .unwrap();
        user.close().await.unwrap();
        let (connector, discovery, _mock) = tisvlay_mock::new_test_parts();
        let restarted = tisvlay_daemon::serve(
            &socket,
            "test",
            vec![
                tisvlay_daemon::VendorReg::new("mock", "Mock restarted", Arc::new(connector))
                    .with_discovery(Arc::new(discovery)),
            ],
            None,
        )
        .await
        .unwrap();
        wait_for_tray(&handle, |tray| {
            tray.view.daemon_alive && tray.view.tvs.is_empty()
        })
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(toast) = notifications.recv().await {
                if toast.summary == "TisVlay daemon is back online" {
                    return;
                }
            }
            panic!("notification capture closed");
        })
        .await
        .unwrap();
        restarted.run_until(std::future::ready(())).await.unwrap();
        system.close().await.unwrap();
    }
}
