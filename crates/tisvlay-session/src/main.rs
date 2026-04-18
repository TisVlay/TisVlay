//! tisvlay-session - User session service for TV control.
//!
//! Monitors desktop D-Bus events (session lock/unlock, idle)
//! and controls TVs through the system daemon (`tisvlayd`).
//!
//! Configuration is split between system and user levels:
//! - System config (daemon-owned): TV definitions, credentials, system default profile
//! - User config (`~/.config/tisvlay/config.toml`): profile definitions, per-TV assignments
//!
//! The session service queries the daemon for the TV list and system default profile,
//! then merges with the user config to produce resolved profiles for each TV.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{DaemonClient, UserConfig, UserProfileConfig};
use tisvlay_session::SessionState;
use tisvlay_session::idle::IdleMonitor;
use tisvlay_session::session_monitor::{SessionEvent, SessionMonitor};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, warn};

/// tisvlay session service - monitors desktop events and controls TVs.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-session", version, about)]
struct Args {
    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    ///
    /// By default, tisvlay-session connects to the system bus to communicate
    /// with tisvlayd. Use this flag when running tisvlayd with --session.
    #[arg(long)]
    user: bool,

    /// Path to the user configuration file.
    ///
    /// Defaults to ~/.config/tisvlay/config.toml (XDG) when not specified.
    /// If the file doesn't exist, the system default profile is used.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tisvlay_core::logging::init(&[
        "tisvlay_session=info",
        "tisvlay_session::volume_sync=debug",
        "tisvlay_session::brightness=debug",
        "zbus=warn",
    ])?;

    let args = Args::parse();

    info!("tisvlay-session starting...");

    // Create daemon client
    let daemon = if args.user {
        info!("Using user/session bus for daemon communication (development mode)");
        DaemonClient::new_session().await
    } else {
        info!("Using system bus for daemon communication");
        DaemonClient::new_system().await
    }
    .context("Failed to create daemon client")?;

    let daemon = Arc::new(daemon);

    // Check if daemon is available (result reused below to decide retry spawn)
    let daemon_available = daemon.is_available().await;
    if !daemon_available {
        warn!("Daemon is not currently available - will retry on events");
    }

    // Load session state (query daemon + load user config)
    let state = match SessionState::load(&daemon, args.config.as_ref()).await {
        Ok(state) => {
            let idle_timeout = state.effective_idle_timeout();
            info!(
                tvs = state.tv_ids.len(),
                idle_timeout = idle_timeout,
                "Session state initialized"
            );
            state
        }
        Err(e) => {
            warn!(error = %e, "Failed to load session state, using empty state");
            SessionState {
                profiles: HashMap::new(),
                user_config: UserConfig::default(),
                admin_user_default: UserProfileConfig::full_default(),
                admin_user_profiles: HashMap::new(),
                tv_ids: Vec::new(),
            }
        }
    };

    let state = Arc::new(RwLock::new(state));

    // Create event channel early so the retry task can replay events
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    // Start session monitor early (it works independently of daemon state).
    // Returns is_locked state for Wayland idle monitor and replay logic.
    let monitor = SessionMonitor::new(event_tx.clone());
    let is_locked = monitor
        .start()
        .await
        .context("Failed to start session monitor")?;

    // If the daemon wasn't available at startup, spawn a background task to
    // retry loading state. This handles the common case where the session
    // service starts before the daemon (e.g., in dev mode or parallel systemd startup).
    // After loading, it replays the current screen state so missed events are recovered.
    if !daemon_available {
        let retry_state = state.clone();
        let retry_daemon = daemon.clone();
        let retry_config = args.config.clone();
        let retry_event_tx = event_tx.clone();
        let retry_is_locked = is_locked.clone();
        tokio::spawn(async move {
            let retry_interval = std::time::Duration::from_secs(5);
            loop {
                tokio::time::sleep(retry_interval).await;
                if !retry_daemon.is_available().await {
                    debug!("Daemon still not available, retrying...");
                    continue;
                }
                info!("Daemon is now available, loading session state...");
                match SessionState::load(&retry_daemon, retry_config.as_ref()).await {
                    Ok(new_state) => {
                        let mut s = retry_state.write().await;
                        *s = new_state;
                        info!(
                            tvs = s.tv_ids.len(),
                            "Session state loaded after daemon became available"
                        );

                        // Replay the current screen state. Events that fired while we had
                        // no TVs were silently dropped. Now that we have TVs, send the
                        // appropriate event so the TV matches the current desktop state.
                        let locked = retry_is_locked.load(std::sync::atomic::Ordering::SeqCst);
                        let replay_event = if locked {
                            SessionEvent::SessionLocked
                        } else {
                            SessionEvent::SessionUnlocked
                        };
                        info!(event = %replay_event, "Replaying current screen state after late daemon connect");
                        let _ = retry_event_tx.send(replay_event);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to load session state, will retry");
                    }
                }
            }
        });
    }

    // Register Session D-Bus interface for tray/GUI to query profiles.
    // The connection must be held alive for the lifetime of the process.
    let _session_dbus_connection = match tisvlay_session::session_dbus::register_session_dbus(
        state.clone(),
        daemon.clone(),
        args.config.clone(),
    )
    .await
    {
        Ok(connection) => {
            info!("Session D-Bus interface available");
            Some(connection)
        }
        Err(e) => {
            warn!(error = %e, "Failed to register session D-Bus interface (profile switching unavailable)");
            None
        }
    };

    // Start Wayland idle monitor for greeter wake detection
    match tisvlay_session::wayland_idle::start_wayland_idle_monitor(event_tx.clone(), is_locked) {
        Ok(()) => {
            info!("Wayland greeter wake detection enabled");
        }
        Err(e) => {
            // Not fatal - Wayland may not be available (X11, SSH, etc.)
            warn!("Wayland greeter wake detection unavailable: {}", e);
        }
    }

    // Start idle monitor if enabled (using minimum non-zero timeout across all profiles)
    let idle_timeout = {
        let s = state.read().await;
        s.effective_idle_timeout()
    };

    if idle_timeout > 0 {
        info!(timeout = idle_timeout, "Starting idle monitor");
        let idle_monitor = IdleMonitor::new(idle_timeout, event_tx.clone());
        tokio::spawn(async move {
            idle_monitor.run().await;
        });
    } else {
        debug!("Idle monitoring disabled (no TVs have idle_timeout_seconds set)");
    }

    // Spawn event handler
    let handler_state = state.clone();
    let handler_daemon = daemon.clone();
    let handler_task = tokio::spawn(async move {
        tisvlay_session::event_handler(event_rx, handler_state, handler_daemon).await;
    });

    // Spawn the volume-sync coordinator. Holds a PipeWire connection
    // for the lifetime of the session, creating / destroying virtual
    // sinks per TV based on the effective user profile.
    let (volume_sync_shutdown_tx, volume_sync_shutdown_rx) = tokio::sync::watch::channel(false);
    let volume_sync_task = {
        let volume_sync_state = state.clone();
        let volume_sync_daemon = daemon.clone();
        Some(tokio::spawn(async move {
            tisvlay_session::volume_sync::run(
                volume_sync_daemon,
                volume_sync_state,
                volume_sync_shutdown_rx,
            )
            .await;
        }))
    };

    // Spawn the brightness-sync coordinator. Dispatches to one of
    // the session-side KDE backends based on the resolved user
    // profile's `sync_backlight` value; exits early if every TV's
    // mode is `None` or `Kernel` (the daemon's kernel-module path
    // owns brightness in that case).
    let (brightness_sync_shutdown_tx, brightness_sync_shutdown_rx) =
        tokio::sync::watch::channel(false);
    let brightness_sync_task = {
        let bs_state = state.clone();
        let bs_daemon = daemon.clone();
        Some(tokio::spawn(async move {
            tisvlay_session::brightness::run(bs_daemon, bs_state, brightness_sync_shutdown_rx)
                .await;
        }))
    };

    // Wait for shutdown signals
    wait_for_signals(state, daemon, args.config).await;

    info!("Shutting down...");

    // Signal coordinators to tear down cleanly.
    let _ = volume_sync_shutdown_tx.send(true);
    let _ = brightness_sync_shutdown_tx.send(true);
    if let Some(task) = volume_sync_task {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    }
    if let Some(task) = brightness_sync_task {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    }

    // Abort the handler task
    handler_task.abort();

    Ok(())
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<PathBuf>,
) {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");
    let mut sighup = signal(SignalKind::hangup()).expect("Failed to register SIGHUP handler");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
                break;
            }
            _ = sigint.recv() => {
                info!("Received SIGINT");
                break;
            }
            _ = sighup.recv() => {
                info!("Received SIGHUP, reloading configuration...");
                match SessionState::load(&daemon, config_path.as_ref()).await {
                    Ok(new_state) => {
                        let idle_timeout = new_state.effective_idle_timeout();
                        let mut s = state.write().await;
                        *s = new_state;
                        info!(
                            idle_timeout = idle_timeout,
                            tvs = s.tv_ids.len(),
                            "Configuration reloaded"
                        );
                        // Note: idle timeout changes require restart to take effect
                        // since the idle monitor is started once at launch
                    }
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                    }
                }
            }
        }
    }
}
