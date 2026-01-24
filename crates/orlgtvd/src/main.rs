//! orlgtvd - LG WebOS TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling LG WebOS TVs.

mod dbus;
mod tv_manager;

use anyhow::{Context, Result};
use clap::Parser;
use orlgtv_core::Config;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::dbus::DaemonService;
use crate::tv_manager::TvManager;

/// D-Bus service name.
pub const DBUS_NAME: &str = "org.orlgtv.Daemon";

/// D-Bus object path.
pub const DBUS_PATH: &str = "/org/orlgtv/Daemon";

/// LG WebOS TV Control Daemon
#[derive(Parser, Debug)]
#[command(name = "orlgtvd", version, about)]
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
        .with(EnvFilter::from_default_env().add_directive("orlgtvd=info".parse()?))
        .init();

    info!("orlgtvd starting...");

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

    // Create TV manager
    let tv_manager = TvManager::new(config);

    // Boot-time WoL for TVs with wake_on_boot enabled
    tv_manager.boot_wake().await;

    // Create D-Bus service
    let service = DaemonService::new(tv_manager);

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

    // Wait for shutdown signal
    wait_for_shutdown().await;

    info!("Shutting down...");

    // Drop connection to unregister from D-Bus
    drop(connection);

    Ok(())
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_shutdown() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {
            info!("Received SIGTERM");
        }
        _ = sigint.recv() => {
            info!("Received SIGINT");
        }
    }
}
