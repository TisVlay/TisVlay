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
use tisvlay_core::{DaemonClient, SessionClient, StateEvent, StateWatcher};
use tray::{ScrollTarget, TisvlayTray, TrayCommand};

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
    // Clone before moving into tray; used to re-queue non-scroll commands
    // that are encountered while draining scroll events in the processor.
    let requeue_tx = cmd_tx.clone();

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

    // Spawn signal watcher (replaces polling)
    let watch_daemon = daemon.clone();
    let watch_session = session.clone();
    let watch_handle = handle.clone();
    tokio::spawn(async move {
        watch_state(watch_daemon, watch_session, watch_handle).await;
    });

    // Spawn command processor
    let cmd_daemon = daemon.clone();
    let cmd_session = session.clone();
    let cmd_handle = handle.clone();
    let cmd_task = tokio::spawn(async move {
        process_commands(cmd_rx, requeue_tx, cmd_daemon, cmd_session, cmd_handle).await;
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
async fn refresh_state(
    daemon: &DaemonClient,
    session: &tokio::sync::RwLock<Option<SessionClient>>,
    handle: &ksni::Handle<TisvlayTray>,
) {
    let session_ref = session.read().await;
    let session_client = session_ref.as_ref();

    match fetch_all_state(daemon, session_client).await {
        Ok(tvs) => {
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
) {
    let mut watcher = match StateWatcher::new(&daemon, Default::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher");
            return;
        }
    };

    // Detach refresh work from the signal-drain loop. If we awaited
    // `refresh_state(...).await` directly here, the loop would not
    // call `watcher.next()` again until the refresh finishes —
    // meaning zbus's signal_stream stops being drained, the bounded
    // channel fills, the I/O task blocks, and eventually method-call
    // replies queue behind unread signal frames in the kernel recv
    // buffer. Result: outstanding tray commands hang waiting for
    // replies that are buried under thousands of signals.
    //
    // Fix: spawn refresh as a separate task and dedupe via an
    // AtomicBool. The watcher loop returns to `next()` immediately,
    // so signal_stream stays drained regardless of refresh latency.
    // At most one refresh is in flight; signals that arrive while a
    // refresh is running collapse into a single "trailing refresh"
    // by re-arming the flag at the end of each task.
    let refresh_in_flight = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let trigger_refresh = |daemon: Arc<DaemonClient>,
                          session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
                          handle: ksni::Handle<TisvlayTray>,
                          flag: Arc<std::sync::atomic::AtomicBool>| {
        use std::sync::atomic::Ordering;
        // Skip if a refresh is already in flight; the in-progress
        // one will pick up the latest state when it completes.
        if flag.swap(true, Ordering::Relaxed) {
            return;
        }
        tokio::spawn(async move {
            refresh_state(&daemon, &session, &handle).await;
            flag.store(false, Ordering::Relaxed);
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
                trigger_refresh(
                    daemon.clone(),
                    session.clone(),
                    handle.clone(),
                    refresh_in_flight.clone(),
                );
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
                        trigger_refresh(
                            daemon.clone(),
                            session.clone(),
                            handle.clone(),
                            refresh_in_flight.clone(),
                        );
                    }
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                trigger_refresh(
                    daemon.clone(),
                    session.clone(),
                    handle.clone(),
                    refresh_in_flight.clone(),
                );
            }
        }
    }
}

/// Process commands from tray menu callbacks.
async fn process_commands(
    mut cmd_rx: mpsc::UnboundedReceiver<TrayCommand>,
    requeue_tx: mpsc::UnboundedSender<TrayCommand>,
    daemon: Arc<DaemonClient>,
    session: Arc<tokio::sync::RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
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
                refresh_state(&daemon, &session, &handle).await;
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
            TrayCommand::ScrollBy(id, target, mut steps) => {
                // Coalesce all pending scroll events for the same TV and target
                // into one D-Bus call, preventing queue buildup from fast scroll wheels.
                loop {
                    match cmd_rx.try_recv() {
                        Ok(TrayCommand::ScrollBy(nid, ntarget, nsteps))
                            if nid == id && ntarget == target =>
                        {
                            steps += nsteps;
                        }
                        Ok(other) => {
                            // Non-scroll command arrived mid-burst: re-queue it so it
                            // isn't lost; it will be processed in the next iteration.
                            let _ = requeue_tx.send(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                if steps == 0 {
                    continue;
                }
                match target {
                    ScrollTarget::Volume => {
                        let current = get_cached_volume(&handle, &id).await;
                        let new = (current as i32 + steps * VOLUME_STEP as i32).clamp(0, 100) as u8;
                        if let Err(e) = daemon.set_volume(&id, new).await {
                            warn!(tv_id = %id, error = %e, "Set volume failed");
                        } else {
                            set_cached_volume(&handle, &id, new).await;
                        }
                    }
                    ScrollTarget::Backlight => {
                        let current = get_cached_backlight(&handle, &id).await;
                        let new =
                            (current as i32 + steps * BACKLIGHT_STEP as i32).clamp(0, 100) as u8;
                        if let Err(e) = daemon.set_backlight(&id, new).await {
                            warn!(tv_id = %id, error = %e, "Set backlight failed");
                        } else {
                            set_cached_backlight(&handle, &id, new).await;
                        }
                    }
                }
                // Skip the full fetch_all_state refresh — the cache was already
                // updated optimistically above, and the next poll cycle will
                // reconcile with the real state. This avoids an extra D-Bus +
                // WebSocket round-trip per scroll step (~5x faster cycle time).
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
