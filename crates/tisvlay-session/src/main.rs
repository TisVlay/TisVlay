//! tisvlay-session - User session service for LG WebOS TV control.
//!
//! Monitors desktop D-Bus events (screen lock/unlock, suspend/resume, idle)
//! and controls TVs through the system daemon (`tisvlayd`).

mod daemon_client;
mod idle;
mod session_monitor;
mod wayland_idle;

use anyhow::{Context, Result};
use clap::Parser;
use daemon_client::DaemonClient;
use idle::IdleMonitor;
use tisvlay_core::{Config, SessionAction};
use session_monitor::{SessionEvent, SessionMonitor};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// tisvlay session service - monitors desktop events and controls TVs.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-session", version, about)]
struct Args {
    /// Use session bus for daemon communication (development mode).
    ///
    /// By default, tisvlay-session connects to the system bus to communicate
    /// with tisvlayd. Use this flag when running tisvlayd with --session.
    #[arg(long)]
    session_daemon: bool,
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
            info!(
                tvs = config.tv.len(),
                idle_timeout = config.session.idle_timeout_seconds,
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
    let daemon = if args.session_daemon {
        info!("Using session bus for daemon communication (development mode)");
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

    // Start idle monitor if enabled
    let idle_timeout = {
        let cfg = config.read().await;
        cfg.session.idle_timeout_seconds
    };

    if idle_timeout > 0 {
        let idle_monitor = IdleMonitor::new(idle_timeout, event_tx.clone());
        tokio::spawn(async move {
            idle_monitor.run().await;
        });
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
async fn event_handler(
    mut event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    config: Arc<RwLock<Config>>,
    daemon: Arc<DaemonClient>,
) {
    info!("Event handler started");

    while let Some(event) = event_rx.recv().await {
        let action = {
            let cfg = config.read().await;
            match event {
                SessionEvent::ScreenLocked => cfg.session.on_screen_lock,
                SessionEvent::ScreenUnlocked => cfg.session.on_screen_unlock,
                SessionEvent::GreeterWake => cfg.session.on_greeter_wake,
                SessionEvent::PrepareForSleep => cfg.session.on_sleep,
                SessionEvent::ResumeFromSleep => cfg.session.on_wake,
                SessionEvent::IdleTimeout => cfg.session.on_idle_timeout,
                SessionEvent::IdleResume => SessionAction::ScreenOn, // Always resume from idle
            }
        };

        info!(event = %event, action = ?action, "Processing session event");

        if action == SessionAction::None {
            debug!("Action is None, skipping");
            continue;
        }

        execute_action(&daemon, action).await;
    }

    info!("Event handler stopped");
}

/// Execute an action on all configured TVs.
async fn execute_action(daemon: &DaemonClient, action: SessionAction) {
    let action_name = format!("{:?}", action);

    let success_count = match action {
        SessionAction::ScreenOff => {
            daemon
                .execute_on_all_tvs(&action_name, |tv_id| {
                    let d = daemon.clone();
                    async move { d.screen_off(&tv_id).await }
                })
                .await
        }
        SessionAction::ScreenOn => {
            daemon
                .execute_on_all_tvs(&action_name, |tv_id| {
                    let d = daemon.clone();
                    async move { d.screen_on(&tv_id).await }
                })
                .await
        }
        SessionAction::PowerOff => {
            daemon
                .execute_on_all_tvs(&action_name, |tv_id| {
                    let d = daemon.clone();
                    async move { d.power_off(&tv_id).await }
                })
                .await
        }
        SessionAction::PowerOn => {
            daemon
                .execute_on_all_tvs(&action_name, |tv_id| {
                    let d = daemon.clone();
                    async move { d.power_on(&tv_id).await }
                })
                .await
        }
        SessionAction::None => 0,
    };

    if success_count > 0 {
        info!(action = %action_name, count = success_count, "Action completed");
    }
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
                        let mut cfg = config.write().await;
                        *cfg = new_config;
                        info!(
                            idle_timeout = cfg.session.idle_timeout_seconds,
                            "Configuration reloaded"
                        );
                    }
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                    }
                }
            }
        }
    }
}
