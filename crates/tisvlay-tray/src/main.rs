//! tisvlay-tray - System tray app for TV control.
//!
//! Provides a StatusNotifierItem (system tray icon) for quick TV control
//! via the tisvlayd D-Bus daemon. Communicates exclusively through
//! `DaemonClient` — no vendor crate dependencies.

mod state;
mod tray;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ksni::TrayMethods;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use state::fetch_all_state;
use tisvlay_core::DaemonClient;
use tray::{TisvlayTray, TrayCommand};

/// tisvlay system tray - TV control from your taskbar.
#[derive(Parser, Debug)]
#[command(name = "tisvlay-tray", version, about)]
struct Args {
    /// Use session bus instead of system D-Bus (e.g. with `tisvlayd --session`)
    #[arg(long)]
    user: bool,
}

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const VOLUME_STEP: u8 = 5;
const BACKLIGHT_STEP: u8 = 10;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(
            EnvFilter::from_default_env()
                .add_directive("tisvlay_tray=info".parse()?)
                .add_directive("zbus=warn".parse()?)
                .add_directive("ksni=warn".parse()?),
        )
        .init();

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

    // Create command channel
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    // Create tray with initial empty state
    let tray = TisvlayTray {
        tvs: Vec::new(),
        daemon_available: false,
        cmd_tx,
    };

    // Spawn the tray icon
    let handle = tray
        .spawn()
        .await
        .context("Failed to spawn tray icon (is a StatusNotifierHost running?)")?;

    info!("Tray icon spawned");

    // Do initial state fetch
    match fetch_all_state(&daemon).await {
        Ok(tvs) => {
            handle
                .update(|t| {
                    t.tvs = tvs;
                    t.daemon_available = true;
                })
                .await;
            info!("Initial state loaded");
        }
        Err(e) => {
            warn!(error = %e, "Daemon not available at startup");
        }
    }

    // Spawn polling task
    let poll_daemon = daemon.clone();
    let poll_handle = handle.clone();
    tokio::spawn(async move {
        poll_state(poll_daemon, poll_handle).await;
    });

    // Spawn command processor
    let cmd_daemon = daemon.clone();
    let cmd_handle = handle.clone();
    let cmd_task = tokio::spawn(async move {
        process_commands(cmd_rx, cmd_daemon, cmd_handle).await;
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
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT");

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

/// Periodically fetch TV state and update the tray menu.
async fn poll_state(daemon: Arc<DaemonClient>, handle: ksni::Handle<TisvlayTray>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if handle.is_closed() {
            break;
        }

        match fetch_all_state(&daemon).await {
            Ok(tvs) => {
                handle
                    .update(|t| {
                        t.tvs = tvs;
                        t.daemon_available = true;
                    })
                    .await;
            }
            Err(e) => {
                debug!(error = %e, "State poll failed");
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
}

/// Process commands from tray menu callbacks.
async fn process_commands(
    mut cmd_rx: mpsc::UnboundedReceiver<TrayCommand>,
    daemon: Arc<DaemonClient>,
    handle: ksni::Handle<TisvlayTray>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        debug!(cmd = ?cmd, "Processing tray command");

        match cmd {
            TrayCommand::Quit => break,
            TrayCommand::Refresh => {}
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
            TrayCommand::SwitchInput(id, input) => {
                if let Err(e) = daemon.switch_input(&id, &input).await {
                    warn!(tv_id = %id, input = %input, error = %e, "Switch input failed");
                }
            }
            TrayCommand::VolumeUp(id) => {
                if let Ok(current) = daemon.get_volume(&id).await {
                    let new = current.saturating_add(VOLUME_STEP).min(100);
                    if let Err(e) = daemon.set_volume(&id, new).await {
                        warn!(tv_id = %id, error = %e, "Set volume failed");
                    }
                }
            }
            TrayCommand::VolumeDown(id) => {
                if let Ok(current) = daemon.get_volume(&id).await {
                    let new = current.saturating_sub(VOLUME_STEP);
                    if let Err(e) = daemon.set_volume(&id, new).await {
                        warn!(tv_id = %id, error = %e, "Set volume failed");
                    }
                }
            }
            TrayCommand::ToggleMute(id) => {
                // Read current state and toggle
                match daemon.get_state(&id).await {
                    Ok(state) => {
                        let mute = !state.muted.unwrap_or(false);
                        if let Err(e) = daemon.set_mute(&id, mute).await {
                            warn!(tv_id = %id, error = %e, "Toggle mute failed");
                        }
                    }
                    Err(e) => warn!(tv_id = %id, error = %e, "Get state failed"),
                }
            }
            TrayCommand::BacklightUp(id) => {
                if let Ok(current) = daemon.get_backlight(&id).await {
                    let new = current.saturating_add(BACKLIGHT_STEP).min(100);
                    if let Err(e) = daemon.set_backlight(&id, new).await {
                        warn!(tv_id = %id, error = %e, "Set backlight failed");
                    }
                }
            }
            TrayCommand::BacklightDown(id) => {
                if let Ok(current) = daemon.get_backlight(&id).await {
                    let new = current.saturating_sub(BACKLIGHT_STEP);
                    if let Err(e) = daemon.set_backlight(&id, new).await {
                        warn!(tv_id = %id, error = %e, "Set backlight failed");
                    }
                }
            }
        }

        // Refresh state after every command so menu reflects the change
        if let Ok(tvs) = fetch_all_state(&daemon).await {
            handle
                .update(|t| {
                    t.tvs = tvs;
                    t.daemon_available = true;
                })
                .await;
        }
    }
}
