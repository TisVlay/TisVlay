//! tisvlayd - TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling TVs.
//! Supports multiple TV vendors through a backend registry system.

mod dbus;
mod registry;
mod tv_manager;

use anyhow::{Context, Result};
use clap::Parser;
use std::sync::Arc;
use tisvlay_core::Config;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use zbus::object_server::SignalEmitter;

use crate::dbus::{
    register_tv_interfaces, tv_path, ManagerService, TvService, DBUS_NAME, MANAGER_PATH,
};
use crate::registry::create_registry;
use crate::tv_manager::{StateChangeEvent, TvManager};

/// TV Control Daemon
#[derive(Parser, Debug)]
#[command(name = "tisvlayd", version, about)]
struct Args {
    /// Use session bus instead of system bus (for development/testing)
    ///
    /// By default, tisvlay connects to the system bus.
    /// Use this flag when running tisvlayd via user session.
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

    // Create backend registry
    let registry = Arc::new(create_registry());
    info!(
        vendors = ?registry.supported_vendors(),
        "Backend registry initialized"
    );

    // Create TV manager
    let (tv_manager, state_change_rx) = TvManager::new(config, registry.clone());

    // Get list of configured TV IDs before wrapping in RwLock
    let tv_ids: Vec<String> = tv_manager.list_tv_ids().await;

    // Wrap manager in Arc<RwLock> for sharing between D-Bus service and signal handler
    let shared_manager = Arc::new(RwLock::new(tv_manager));

    // Boot-time WoL for TVs with wake_on_boot enabled
    {
        let manager = shared_manager.read().await;
        manager.boot_wake().await;
    }

    // Connect to D-Bus (session or system bus)
    let builder = if args.session {
        info!("Using session bus (development mode)");
        zbus::connection::Builder::session().context("Failed to connect to session D-Bus")?
    } else {
        info!("Using system bus");
        zbus::connection::Builder::system().context("Failed to connect to system D-Bus")?
    };

    // Build connection and request name
    let connection = builder
        .name(DBUS_NAME)
        .context("Failed to request D-Bus name")?
        .build()
        .await
        .context("Failed to build D-Bus connection")?;

    // Create and register manager service at /org/tisvlay/Manager
    let manager_service = ManagerService::new(shared_manager.clone(), registry.clone());
    connection
        .object_server()
        .at(MANAGER_PATH, manager_service)
        .await
        .context("Failed to serve Manager interface")?;
    info!("Manager interface registered at {}", MANAGER_PATH);

    // Register per-TV D-Bus objects at /org/tisvlay/Tv/{id}
    for tv_id in &tv_ids {
        let path = tv_path(tv_id);
        register_tv_interfaces(&connection, &path, tv_id.clone(), shared_manager.clone())
            .await
            .with_context(|| format!("Failed to serve TV interface at {}", path))?;
        info!(tv_id = %tv_id, path = %path, "TV interface registered");
    }

    info!("D-Bus service registered as {}", DBUS_NAME);

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
        debug!(tv_id = %event.tv_id, "Emitting state change signals");

        // Emit on the per-TV object path
        let path = tv_path(&event.tv_id);
        match SignalEmitter::new(&connection, path.as_str()) {
            Ok(emitter) => {
                // Emit StateChanged signal
                if let Err(e) = TvService::state_changed(&emitter, &event.state_json).await {
                    warn!(error = %e, tv_id = %event.tv_id, "Failed to emit StateChanged signal");
                }
            }
            Err(e) => {
                warn!(error = %e, tv_id = %event.tv_id, "Failed to create signal emitter");
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
