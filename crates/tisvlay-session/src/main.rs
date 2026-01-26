//! tisvlay-session - User session service for TV control.
//!
//! Monitors desktop D-Bus events (screen lock/unlock, suspend/resume, idle)
//! and controls TVs through the system daemon (`tisvlayd`).
//!
//! Each TV can have its own profile that defines different behaviors for
//! system events. See the configuration documentation for details.

mod idle;
mod session_monitor;
mod wayland_idle;

use anyhow::{Context, Result};
use clap::Parser;
use idle::IdleMonitor;
use tisvlay_core::{Config, DaemonClient, ProfileConfig, SessionAction};
use session_monitor::{SessionEvent, SessionMonitor};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(
            EnvFilter::from_default_env()
                .add_directive("tisvlay_session=info".parse()?)
                .add_directive("zbus=warn".parse()?),
        )
        .init();

    let args = Args::parse();

    info!("tisvlay-session starting...");

    // Load configuration
    let config = match Config::load() {
        Ok(config) => {
            // Calculate effective idle timeout (minimum non-zero across all TV profiles)
            let idle_timeout = effective_idle_timeout(&config);
            info!(
                tvs = config.tv.len(),
                idle_timeout = idle_timeout,
                "Loaded configuration"
            );
            config
        }
        Err(e) => {
            warn!("Failed to load config, using defaults: {}", e);
            Config::default()
        }
    };

    let config = Arc::new(RwLock::new(config));

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

    // Check if daemon is available
    if !daemon.is_available().await {
        warn!("Daemon is not currently available - will retry on events");
    }

    // Create event channel
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    // Start session monitor (returns is_locked state for Wayland idle monitor)
    let monitor = SessionMonitor::new(event_tx.clone());
    let is_locked = monitor
        .start()
        .await
        .context("Failed to start session monitor")?;

    // Start Wayland idle monitor for greeter wake detection
    match wayland_idle::start_wayland_idle_monitor(event_tx.clone(), is_locked) {
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
        let cfg = config.read().await;
        effective_idle_timeout(&cfg)
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
    let handler_config = config.clone();
    let handler_daemon = daemon.clone();
    let handler_task = tokio::spawn(async move {
        event_handler(event_rx, handler_config, handler_daemon).await;
    });

    // Wait for shutdown signals
    wait_for_signals(config).await;

    info!("Shutting down...");

    // Abort the handler task
    handler_task.abort();

    Ok(())
}

/// Handle session events by executing configured actions.
///
/// For each event, we iterate through all configured TVs and apply the action
/// defined in each TV's profile. This allows different TVs to respond differently
/// to the same event (e.g., gaming TV ignores screen lock, desk TV blanks).
async fn event_handler(
    mut event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    config: Arc<RwLock<Config>>,
    daemon: Arc<DaemonClient>,
) {
    info!("Event handler started");

    while let Some(event) = event_rx.recv().await {
        info!(event = %event, "Processing session event");

        // Get TV configurations and their profiles
        let tv_actions: Vec<(String, SessionAction)> = {
            let cfg = config.read().await;
            cfg.tv
                .iter()
                .map(|tv| {
                    let profile = cfg.get_profile_for_tv(tv);
                    let action = get_action_for_event(&event, &profile);
                    (tv.id.clone(), action)
                })
                .collect()
        };

        // Execute action for each TV
        let mut success_count = 0;
        for (tv_id, action) in tv_actions {
            if action == SessionAction::None {
                debug!(tv_id = %tv_id, "Action is None, skipping");
                continue;
            }

            debug!(tv_id = %tv_id, action = ?action, "Executing action");
            if let Err(e) = execute_action_for_tv(&daemon, &tv_id, action).await {
                warn!(tv_id = %tv_id, error = %e, "Action failed");
            } else {
                success_count += 1;
            }
        }

        if success_count > 0 {
            info!(event = %event, count = success_count, "Event processed");
        }
    }

    info!("Event handler stopped");
}

/// Get the action for a session event from a profile.
fn get_action_for_event(event: &SessionEvent, profile: &ProfileConfig) -> SessionAction {
    match event {
        SessionEvent::ScreenLocked => profile.on_screen_lock,
        SessionEvent::ScreenUnlocked => profile.on_screen_unlock,
        SessionEvent::GreeterWake => profile.on_greeter_wake,
        SessionEvent::PrepareForSleep => profile.on_sleep,
        SessionEvent::ResumeFromSleep => profile.on_wake,
        SessionEvent::IdleTimeout => profile.on_idle_timeout,
        SessionEvent::IdleResume => SessionAction::ScreenOn, // Always resume from idle
    }
}

/// Execute an action on a specific TV.
async fn execute_action_for_tv(
    daemon: &DaemonClient,
    tv_id: &str,
    action: SessionAction,
) -> anyhow::Result<()> {
    match action {
        SessionAction::ScreenOff => daemon.screen_off(tv_id).await,
        SessionAction::ScreenOn => daemon.screen_on(tv_id).await,
        SessionAction::PowerOff => daemon.power_off(tv_id).await,
        SessionAction::PowerOn => daemon.power_on(tv_id).await,
        SessionAction::None => Ok(()),
    }
}

/// Calculate the effective idle timeout for the session service.
///
/// Returns the minimum non-zero idle_timeout_seconds across all TV profiles.
/// If all profiles have idle disabled (0), returns 0.
fn effective_idle_timeout(config: &Config) -> u64 {
    config
        .tv
        .iter()
        .map(|tv| config.get_profile_for_tv(tv).idle_timeout_seconds)
        .filter(|&timeout| timeout > 0)
        .min()
        .unwrap_or(0)
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(config: Arc<RwLock<Config>>) {
    use tokio::signal::unix::{signal, SignalKind};

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
                match Config::load() {
                    Ok(new_config) => {
                        let idle_timeout = effective_idle_timeout(&new_config);
                        let mut cfg = config.write().await;
                        *cfg = new_config;
                        info!(
                            idle_timeout = idle_timeout,
                            tvs = cfg.tv.len(),
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
