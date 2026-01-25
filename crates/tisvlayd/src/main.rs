//! tisvlayd - LG WebOS TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling LG WebOS TVs.

mod dbus;
mod tv_manager;

use anyhow::{Context, Result};
use clap::Parser;
use tisvlay_core::Config;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use zbus::object_server::SignalEmitter;

use crate::dbus::DaemonService;
use crate::tv_manager::{StateChangeEvent, TvManager};

/// D-Bus service name.
pub const DBUS_NAME: &str = "org.tisvlay.Daemon";

/// D-Bus object path.
pub const DBUS_PATH: &str = "/org/tisvlay/Daemon";

/// LG WebOS TV Control Daemon
#[derive(Parser, Debug)]
#[command(name = "tisvlayd", version, about)]
struct Args {
    /// Use session bus instead of system bus (for development/testing)
    #[arg(long)]
    session: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env().add_directive("tisvlayd=info".parse()?))
        .init();

    info!("tisvlayd starting...");

    // Load configuration
    let config = match Config::load() {
        Ok(config) => {
            info!(tvs = config.tv.len(), "Loaded configuration");
            config
        }
        Err(e) => {
            warn!("Failed to load config, using defaults: {}", e);
            Config::default()
        }
    };

    // Create TV manager (returns manager, reconnection worker handle, and state change receiver)
    let (tv_manager, reconnect_worker, state_change_rx) = TvManager::new(config);

    // Wrap manager in Arc<RwLock> for sharing between D-Bus service and signal handler
    let shared_manager = Arc::new(RwLock::new(tv_manager));

    // Boot-time WoL for TVs with wake_on_boot enabled
    {
        let manager = shared_manager.read().await;
        manager.boot_wake().await;
    }

    // Create D-Bus service with shared manager
    let service = DaemonService::new_with_shared(shared_manager.clone());

    // Connect to D-Bus (session or system bus)
    let builder = if args.session {
        info!("Using session bus (development mode)");
        zbus::connection::Builder::session().context("Failed to connect to session D-Bus")?
    } else {
        info!("Using system bus");
        zbus::connection::Builder::system().context("Failed to connect to system D-Bus")?
    };

    let connection = builder
        .name(DBUS_NAME)
        .context("Failed to request D-Bus name")?
        .serve_at(DBUS_PATH, service)
        .context("Failed to serve D-Bus interface")?
        .build()
        .await
        .context("Failed to build D-Bus connection")?;

    info!("D-Bus service registered at {}", DBUS_NAME);

    // Spawn task to emit state change signals
    let signal_worker = tokio::spawn(state_change_signal_emitter(
        connection.clone(),
        state_change_rx,
    ));

    // Wait for shutdown or reload signal
    wait_for_signals(shared_manager).await;

    info!("Shutting down...");

    // Abort the signal worker
    signal_worker.abort();

    // Abort the reconnection worker
    reconnect_worker.abort();

    // Drop connection to unregister from D-Bus
    drop(connection);

    Ok(())
}

/// Emit state change signals on the D-Bus.
async fn state_change_signal_emitter(
    connection: zbus::Connection,
    mut rx: mpsc::UnboundedReceiver<StateChangeEvent>,
) {
    info!("State change signal emitter started");

    while let Some(event) = rx.recv().await {
        debug!(tv_id = %event.tv_id, "Emitting TvStateChanged signal");

        // Create signal emitter for the D-Bus object path
        match SignalEmitter::new(&connection, DBUS_PATH) {
            Ok(emitter) => {
                if let Err(e) =
                    DaemonService::tv_state_changed(&emitter, &event.tv_id, &event.state_json).await
                {
                    warn!(error = %e, "Failed to emit TvStateChanged signal");
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to create signal emitter");
            }
        }
    }

    info!("State change signal emitter stopped");
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(manager: Arc<RwLock<TvManager>>) {
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
                let mgr = manager.read().await;
                if let Err(e) = mgr.reload_config().await {
                    warn!("Failed to reload config: {}", e);
                }
            }
        }
    }
}
