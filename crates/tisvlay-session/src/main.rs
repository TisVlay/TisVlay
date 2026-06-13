//! tisvlay-session - User session service for TV control.
//!
//! Monitors desktop D-Bus events (session lock/unlock, idle)
//! and controls TVs through the system daemon (`tisvlayd`).
//!
//! Configuration is split between system and user levels:
//! - System config (daemon-owned): TV definitions, credentials, system default profile
//! - User config (`~/.config/tisvlay/session.toml`): profile definitions, per-TV assignments
//!
//! The session service queries the daemon for the TV list and system default profile,
//! then merges with the user config to produce resolved profiles for each TV.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{DaemonClient, UserConfig, UserProfileConfig, absolutize};
use tisvlay_session::SessionState;
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
    /// Defaults to ~/.config/tisvlay/session.toml (XDG) when not specified.
    /// If the file doesn't exist, the system default profile is used.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_session=info", "zbus=warn"], &[])?;

    let args = Args::parse();

    info!("tisvlay-session starting...");

    // Absolutize a relative `--config` argument against the session's
    // CWD at startup. The `ConfigPath` D-Bus property must be
    // absolute so consumers (GUI's open-folder action, sysadmins,
    // monitoring tools) can resolve it without guessing our CWD.
    // `None` means "default to XDG", which the SessionService falls
    // back to via `UserConfig::config_path()` (already absolute).
    let config_path = args.config.map(absolutize);

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
    let state = match SessionState::load(&daemon, config_path.as_ref()).await {
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
        let retry_config = config_path.clone();
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
        config_path.clone(),
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

    // Compute the effective in-session idle timeout (minimum non-zero across
    // all TVs; `None` = disabled). Fixed at launch — a reload requires a
    // restart to re-register the notification (see the SIGHUP handler).
    let idle_timeout_ms = {
        let s = state.read().await;
        let secs = s.effective_idle_timeout();
        (secs > 0).then(|| secs.saturating_mul(1000).min(u32::MAX as u64) as u32)
    };

    // Start the Wayland presence watches: lock-wake (always) plus the
    // in-session idle watch when a timeout is configured.
    match tisvlay_session::wayland_idle::start_wayland_idle_monitor(
        event_tx.clone(),
        is_locked,
        idle_timeout_ms,
    ) {
        Ok(()) => {
            info!(idle_timeout_ms = ?idle_timeout_ms, "Wayland presence detection enabled");
        }
        Err(e) => {
            // Not fatal - Wayland may not be available (X11, SSH, etc.)
            warn!("Wayland presence detection unavailable: {}", e);
        }
    }

    // Spawn event handler
    let handler_state = state.clone();
    let handler_daemon = daemon.clone();
    let handler_task = tokio::spawn(async move {
        tisvlay_session::event_handler(event_rx, handler_state, handler_daemon).await;
    });

    // Spawn the orphan-pruner: when the daemon unregisters a TV's
    // `Tv1.*` interfaces (via `Tv1.Remove()`), this watches the
    // resulting `ObjectManager.InterfacesRemoved` signal and prunes
    // the corresponding user-config `[[tv]]` assignment so we don't
    // accumulate orphans across the device lifecycle.
    let pruner_task = match daemon.tv_list_change_stream().await {
        Ok(stream) => {
            let pruner_state = state.clone();
            let pruner_daemon = daemon.clone();
            let pruner_config = config_path.clone();
            Some(tokio::spawn(async move {
                use futures_util::StreamExt;
                let mut stream = std::pin::pin!(stream);
                while stream.next().await.is_some() {
                    tisvlay_session::reconcile_user_config_orphans(
                        &pruner_state,
                        &pruner_daemon,
                        pruner_config.as_ref(),
                    )
                    .await;
                }
            }))
        }
        Err(e) => {
            warn!(
                error = %e,
                "Failed to subscribe to daemon TV-list signals; orphan user config entries will not be pruned automatically"
            );
            None
        }
    };

    // Wait for shutdown signals
    wait_for_signals(state, daemon, config_path).await;

    info!("Shutting down...");

    // Abort spawned tasks
    handler_task.abort();
    if let Some(t) = pruner_task {
        t.abort();
    }

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
