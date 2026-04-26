//! tisvlay-tray - System tray app for TV control.
//!
//! Provides a StatusNotifierItem (system tray icon) for quick TV control
//! via the tisvlayd D-Bus daemon. Communicates exclusively through
//! `DaemonClient` — no vendor crate dependencies.

mod state;
mod tray;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use ksni::TrayMethods;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use state::fetch_all_state;
use tisvlay_core::{
    Coalescer, DaemonClient, ReconcilerMap, RefreshTrigger, SessionClient, StateEvent, StateWatcher,
};
use tray::{ScrollTarget, TisvlayTray, TrayCommand};

/// Per-(tv_id, property) reconciler maps for scroll-driven values.
/// Bundles the volume and backlight maps so the apply layer can be
/// passed a single `Reconcilers` value through `process_commands`,
/// `watch_state`, and `refresh_state` without two parameters
/// everywhere.
///
/// The apply uses `volume.current()` / `backlight.current()` as the
/// base for the next delta (pinned to user intent) and `dispatch()`
/// to update intent on each user input. The refresh path feeds
/// every fetched value through `observe()`, gating mid-ramp values
/// that don't match the last dispatch. See
/// `crates/tisvlayd/tests/scroll_apply_under_ramp.rs` for the
/// regression test that this gating prevents.
#[derive(Clone, Default, Debug)]
struct Reconcilers {
    volume: ReconcilerMap<String, u8>,
    backlight: ReconcilerMap<String, u8>,
}

/// tisvlay system tray - TV control from your taskbar.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-tray", version, about)]
struct Args {
    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long)]
    user: bool,
}

const VOLUME_STEP: u8 = 1;
const BACKLIGHT_STEP: u8 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_tray=info", "zbus=warn", "ksni=warn"])?;

    let args = Args::parse();

    info!("tisvlay-tray starting...");

    // Create daemon client
    let daemon = if args.user {
        info!("Using user/session bus (development mode)");
        DaemonClient::new_session().await
    } else {
        info!("Using system bus");
        DaemonClient::new_system().await
    }
    .context("Failed to create daemon client")?;

    let daemon = Arc::new(daemon);

    // Session client for profile management — eagerly connected if available,
    // then lazily retried on each poll if not.
    let session: Arc<tokio::sync::RwLock<Option<SessionClient>>> =
        Arc::new(tokio::sync::RwLock::new(match SessionClient::new().await {
            Ok(client) if client.is_available().await => {
                info!("Session service available for profile management");
                Some(client)
            }
            _ => None,
        }));

    // Fetch initial state before creating the tray so it starts with full
    // data. This prevents the first poll from triggering a ksni LayoutUpdated
    // signal that would close any submenu the user opened early.
    let (initial_tvs, initial_available) = {
        let session_ref = session.read().await;
        match fetch_all_state(&daemon, session_ref.as_ref()).await {
            Ok(tvs) => (tvs, true),
            Err(e) => {
                debug!(error = %e, "Initial state fetch failed");
                (Vec::new(), false)
            }
        }
    };

    // Create command channel
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    // Create tray with initial state
    let tray = TisvlayTray {
        tvs: initial_tvs,
        daemon_available: initial_available,
        cmd_tx,
        scroll_target: Default::default(),
        scroll_tv_id: None,
    };

    // Spawn the tray icon
    let handle = tray
        .spawn()
        .await
        .context("Failed to spawn tray icon (is a StatusNotifierHost running?)")?;

    info!("Tray icon spawned");

    // Per-(tv_id) reconcilers for scroll-driven properties. Shared
    // between the watch_state refresh path and the scroll apply
    // closures so the apply's base value (`current + delta`) and the
    // menu's displayed value both stay pinned to the user's intent
    // during TV ramps. See `Reconcilers` docstring above.
    let reconcilers = Reconcilers::default();

    // Spawn signal watcher (replaces polling)
    let watch_daemon = daemon.clone();
    let watch_session = session.clone();
    let watch_handle = handle.clone();
    let watch_reconcilers = reconcilers.clone();
    tokio::spawn(async move {
        watch_state(watch_daemon, watch_session, watch_handle, watch_reconcilers).await;
    });

    // Per-(tv_id) sum-merge coalescers for scroll-wheel deltas. Folds
    // a burst of `ScrollBy` events into a single daemon write per TV
    // per target. The apply uses `Reconcilers` to read the
    // user's intent (not the bouncing daemon cache during a TV ramp)
    // as the base for `current + delta`, then dispatches the new
    // intent BEFORE awaiting the daemon round-trip so the gate is in
    // place before any subscription pushes arrive.

    let volume_scroll = Coalescer::new(|old: i32, new: i32| old + new, {
        let daemon = daemon.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        move |tv_id: String, steps: i32| {
            let daemon = daemon.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = get_cached_volume(&handle, &tv_id).await;
                let current = reconcilers.volume.current(&tv_id, fallback);
                let new = (current as i32 + steps * VOLUME_STEP as i32).clamp(0, 100) as u8;
                reconcilers.volume.dispatch(&tv_id, new);
                if let Err(e) = daemon.set_volume(&tv_id, new).await {
                    warn!(tv_id = %tv_id, error = %e, "Set volume failed");
                } else {
                    set_cached_volume(&handle, &tv_id, new).await;
                }
            }
        }
    });
    let backlight_scroll = Coalescer::new(|old: i32, new: i32| old + new, {
        let daemon = daemon.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        move |tv_id: String, steps: i32| {
            let daemon = daemon.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move {
                let fallback = get_cached_backlight(&handle, &tv_id).await;
                let current = reconcilers.backlight.current(&tv_id, fallback);
                let new = (current as i32 + steps * BACKLIGHT_STEP as i32).clamp(0, 100) as u8;
                reconcilers.backlight.dispatch(&tv_id, new);
                if let Err(e) = daemon.set_backlight(&tv_id, new).await {
                    warn!(tv_id = %tv_id, error = %e, "Set backlight failed");
                } else {
                    set_cached_backlight(&handle, &tv_id, new).await;
                }
            }
        }
    });

    // Spawn command processor
    let cmd_daemon = daemon.clone();
    let cmd_session = session.clone();
    let cmd_handle = handle.clone();
    let cmd_reconcilers = reconcilers.clone();
    let cmd_task = tokio::spawn(async move {
        process_commands(
            cmd_rx,
            cmd_daemon,
            cmd_session,
            cmd_handle,
            volume_scroll,
            backlight_scroll,
            cmd_reconcilers,
        )
        .await;
    });

    // Wait for shutdown: command processor exit (Quit menu item) or signal
    tokio::select! {
        _ = cmd_task => {}
        _ = wait_for_signal() => {
            info!("Received shutdown signal");
        }
    }

    info!("Shutting down...");
    handle.shutdown();

    Ok(())
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT");

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

/// Fetch TV state and update the tray menu immediately.
///
/// Each TV's `volume` and `backlight` are routed through
/// `reconcilers.{volume,backlight}.observe` before being written to `CachedTvState`,
/// so mid-ramp values that don't match a pending dispatch don't
/// bounce the displayed/menu value (or the apply layer's base for
/// the next delta). Other fields (connected, current_input, etc.)
/// pass through unchanged — they aren't delta-driven and don't have
/// transient intermediate states to gate.
async fn refresh_state(
    daemon: &DaemonClient,
    session: &tokio::sync::RwLock<Option<SessionClient>>,
    handle: &ksni::Handle<TisvlayTray>,
    reconcilers: &Reconcilers,
) {
    let session_ref = session.read().await;
    let session_client = session_ref.as_ref();

    match fetch_all_state(daemon, session_client).await {
        Ok(mut tvs) => {
            for tv in &mut tvs {
                if let Some(v) = tv.volume {
                    tv.volume = Some(reconcilers.volume.observe(&tv.id, v));
                }
                if let Some(b) = tv.backlight {
                    tv.backlight = Some(reconcilers.backlight.observe(&tv.id, b));
                }
            }
            debug!(count = tvs.len(), "Refresh: state updated");
            handle
                .update(|t| {
                    t.tvs = tvs;
                    t.daemon_available = true;
                })
                .await;
        }
        Err(e) => {
            debug!(error = %e, "Refresh: state fetch failed");
            handle
                .update(|t| {
                    t.daemon_available = false;
                    for tv in &mut t.tvs {
                        tv.connected = false;
                    }
                })
                .await;
        }
    }
}

/// Watch for daemon D-Bus signals and refresh the tray on state changes.
///
/// Delegates all D-Bus subscription and lifecycle watching to
/// [`StateWatcher`], which handles signal debouncing, service name
/// monitoring, and fallback polling. This function only contains the
/// tray-specific responses to each event.
async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
    reconcilers: Reconcilers,
) {
    let mut watcher = match StateWatcher::new(&daemon, Default::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher");
            return;
        }
    };

    // `watcher.next() → refresh.await` inline halts signal_stream
    // draining and wedges the connection under flood. See
    // `RefreshTrigger`'s docstring for the bug history.
    let trigger = RefreshTrigger::new();
    let trigger_refresh = || {
        let daemon = daemon.clone();
        let session = session.clone();
        let handle = handle.clone();
        let reconcilers = reconcilers.clone();
        trigger.fire(move || {
            let daemon = daemon.clone();
            let session = session.clone();
            let handle = handle.clone();
            let reconcilers = reconcilers.clone();
            async move { refresh_state(&daemon, &session, &handle, &reconcilers).await }
        });
    };

    loop {
        if handle.is_closed() {
            break;
        }

        let Some(event) = watcher.next().await else {
            break;
        };

        match event {
            StateEvent::DaemonStateChanged(_)
            | StateEvent::DaemonAppeared
            | StateEvent::SessionProfileChanged
            | StateEvent::FallbackPoll => {
                trigger_refresh();
            }
            StateEvent::DaemonDisappeared => {
                handle
                    .update(|tray: &mut TisvlayTray| {
                        tray.daemon_available = false;
                        tray.tvs.clear();
                    })
                    .await;
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await {
                    if client.is_available().await {
                        *session.write().await = Some(client);
                        trigger_refresh();
                    }
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                trigger_refresh();
            }
        }
    }
}

/// Process commands from tray menu callbacks.
async fn process_commands(
    mut cmd_rx: mpsc::UnboundedReceiver<TrayCommand>,
    daemon: Arc<DaemonClient>,
    session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
    volume_scroll: Coalescer<String, i32>,
    backlight_scroll: Coalescer<String, i32>,
    reconcilers: Reconcilers,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        debug!(cmd = ?cmd, "Processing tray command");

        match cmd {
            TrayCommand::Quit => break,
            TrayCommand::Refresh => {
                // Reload config on both services, then immediately refresh state
                if let Err(e) = daemon.reload_config().await {
                    warn!(error = %e, "Refresh: daemon reload failed");
                }
                let session_ref = session.read().await;
                if let Some(ref session_client) = *session_ref {
                    if let Err(e) = session_client.reload_config().await {
                        warn!(error = %e, "Refresh: session reload failed");
                    }
                }
                drop(session_ref);
                refresh_state(&daemon, &session, &handle, &reconcilers).await;
            }
            TrayCommand::PowerOn(id) => {
                if let Err(e) = daemon.power_on(&id).await {
                    warn!(tv_id = %id, error = %e, "Power on failed");
                }
            }
            TrayCommand::PowerOff(id) => {
                if let Err(e) = daemon.power_off(&id).await {
                    warn!(tv_id = %id, error = %e, "Power off failed");
                }
            }
            TrayCommand::ScreenOn(id) => {
                if let Err(e) = daemon.screen_on(&id).await {
                    warn!(tv_id = %id, error = %e, "Screen on failed");
                }
            }
            TrayCommand::ScreenOff(id) => {
                if let Err(e) = daemon.screen_off(&id).await {
                    warn!(tv_id = %id, error = %e, "Screen off failed");
                }
            }
            TrayCommand::SetUserProfile(id, profile) => {
                let session_ref = session.read().await;
                if let Some(ref session_client) = *session_ref {
                    if let Err(e) = session_client.set_tv_user_profile(&id, &profile).await {
                        warn!(tv_id = %id, profile = %profile, error = %e, "Set user profile failed");
                    }
                } else {
                    warn!(tv_id = %id, "Cannot set user profile: session service not available");
                }
            }
            TrayCommand::SetSystemProfile(id, profile) => {
                if let Err(e) = daemon.set_tv_option(&id, "profile.system", &profile).await {
                    warn!(tv_id = %id, profile = %profile, error = %e, "Set system profile failed");
                }
            }
            TrayCommand::SwitchInput(id, input) => {
                if let Err(e) = daemon.switch_input(&id, &input).await {
                    warn!(tv_id = %id, input = %input, error = %e, "Switch input failed");
                }
            }
            TrayCommand::ScrollBy(id, target, steps) => {
                // Hand off to the per-target sum-merge `Coalescer`. It
                // folds rapid scroll events for the same TV into one
                // daemon write, applying the cached-value lookup +
                // optimistic write inside its apply closure (set up in
                // `main`). We don't await the dispatch here, so other
                // commands aren't blocked behind a slow daemon call.
                if steps == 0 {
                    continue;
                }
                match target {
                    ScrollTarget::Volume => volume_scroll.fire(id, steps),
                    ScrollTarget::Backlight => backlight_scroll.fire(id, steps),
                }
                // Skip the post-command refresh — the coalescer's
                // optimistic cache write keeps the menu in sync, and
                // the next D-Bus signal reconciles for free.
                continue;
            }
            TrayCommand::ToggleMute(id) => {
                // Read from cache to avoid race with rapid toggles
                let currently_muted = get_cached_muted(&handle, &id).await;
                let new_mute = !currently_muted;
                if let Err(e) = daemon.set_mute(&id, new_mute).await {
                    warn!(tv_id = %id, error = %e, "Toggle mute failed");
                } else {
                    set_cached_muted(&handle, &id, new_mute).await;
                }
            }
            TrayCommand::SetManaged(id, managed) => {
                if let Err(e) = daemon
                    .set_tv_option(&id, "managed", if managed { "true" } else { "false" })
                    .await
                {
                    warn!(tv_id = %id, managed = %managed, error = %e, "Set managed failed");
                }
                // No need to notify the session service: it queries the daemon's managed
                // set live on each event, and set_tv_option updates in-memory state
                // synchronously before returning, so the next event dispatch picks it up.
            }
        }

        // Refresh state after command so menu reflects the change
        let session_ref = session.read().await;
        if let Ok(tvs) = fetch_all_state(&daemon, session_ref.as_ref()).await {
            handle
                .update(|t| {
                    t.tvs = tvs;
                    t.daemon_available = true;
                })
                .await;
        }
    }
}

/// Read volume from cached tray state.
async fn get_cached_volume(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> u8 {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.volume)
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
}

/// Optimistically update cached volume.
async fn set_cached_volume(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, volume: u8) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.volume = Some(volume);
            }
        })
        .await;
}

/// Read backlight from cached tray state.
async fn get_cached_backlight(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> u8 {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.backlight)
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
}

/// Optimistically update cached backlight.
async fn set_cached_backlight(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, level: u8) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.backlight = Some(level);
            }
        })
        .await;
}

/// Read muted state from cached tray state.
async fn get_cached_muted(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> bool {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            t.tvs
                .iter()
                .find(|tv| tv.id == id)
                .and_then(|tv| tv.muted)
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
}

/// Optimistically update cached muted state.
async fn set_cached_muted(handle: &ksni::Handle<TisvlayTray>, tv_id: &str, muted: bool) {
    let id = tv_id.to_string();
    handle
        .update(|t| {
            if let Some(tv) = t.tvs.iter_mut().find(|tv| tv.id == id) {
                tv.muted = Some(muted);
            }
        })
        .await;
}
