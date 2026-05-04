//! tisvlayd - TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling TVs.
//! Supports multiple TV vendors through a backend registry system.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{SystemConfig, UserConfig, absolutize};
use tracing::{info, warn};

use tisvlayd::dbus::{BusKind, DBUS_NAME, ManagerService, build_dbus_connection};
use tisvlayd::registry::create_registry;
use tisvlayd::state_forwarder::state_change_forwarder;
use tisvlayd::tv_manager::TvManager;
use tisvlayd::{logind, netlink};

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

    /// Path to the configuration file.
    ///
    /// Defaults to /var/lib/tisvlay/config.toml (system install) or
    /// ~/.config/tisvlay/config.toml when --session is used without --config.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tisvlay_core::logging::init(&["tisvlayd=info", "zbus=warn"])?;

    info!("tisvlayd starting...");

    // Determine config file path. Absolutize relative `--config`
    // arguments against the daemon's CWD at startup so the
    // `ConfigPath` D-Bus property is always an absolute path —
    // consumers (GUI's open-folder action, sysadmins, monitoring
    // tools) can't resolve a relative path against our CWD without
    // racing or guessing.
    let config_path = absolutize(args.config.unwrap_or_else(|| {
        if args.session {
            UserConfig::config_path()
                .unwrap_or_else(|_| PathBuf::from("/var/lib/tisvlay/config.toml"))
        } else {
            PathBuf::from("/var/lib/tisvlay/config.toml")
        }
    }));
    info!(path = %config_path.display(), "Using config file");

    // Load configuration
    let config = match SystemConfig::load_from(&config_path) {
        Ok(config) => {
            info!(tvs = config.tv.len(), "Loaded configuration");
            config
        }
        Err(e) => {
            warn!("Failed to load config, using defaults: {}", e);
            SystemConfig::default()
        }
    };

    // Create backend registry
    let registry = Arc::new(create_registry());
    info!(
        vendors = ?registry.supported_vendors(),
        "Backend registry initialized"
    );

    // Create TV manager
    let (tv_manager, state_change_rx) = TvManager::new(config, config_path, registry.clone());

    // Get list of configured TV IDs
    let tv_ids: Vec<String> = tv_manager.list_tv_ids().await;

    // Wrap manager in Arc for sharing between D-Bus services.
    // TvManager handles all internal synchronization via per-TV mutexes.
    let shared_manager = Arc::new(tv_manager);

    // Create manager D-Bus service (before building connection so it's
    // registered via serve_at before the name is claimed).
    let manager_service = ManagerService::new(shared_manager.clone(), registry.clone());

    // Build D-Bus connection with all static and per-TV interfaces
    // registered before the well-known name is claimed. Tests reach
    // the same code path via `BusKind::Address(...)` against a private
    // dbus-daemon.
    if args.session {
        info!("Using session bus (development mode)");
    } else {
        info!("Using system bus");
    }
    let bus_kind = if args.session {
        BusKind::Session
    } else {
        BusKind::System
    };
    let connection =
        build_dbus_connection(bus_kind, manager_service, &tv_ids, shared_manager.clone()).await?;

    info!(
        "D-Bus service registered as {} ({} TVs registered)",
        DBUS_NAME,
        tv_ids.len()
    );

    // Execute on_startup action (e.g., power_on wakes TVs via WoL or other transport).
    // Runs in the background so D-Bus is immediately reachable.
    shared_manager.on_startup();

    // Start logind monitoring (shutdown inhibitor, session tracking)
    let logind_monitor = logind::LogindMonitor::new(shared_manager.clone(), args.session);
    if let Err(e) = logind_monitor.start().await {
        warn!(error = %e, "Failed to start logind monitor (system lifecycle events unavailable)");
    }

    // Spawn task to forward state changes to D-Bus properties and signals
    let signal_worker = tokio::spawn(state_change_forwarder(
        connection.clone(),
        state_change_rx,
        shared_manager.clone(),
    ));

    // Spawn kernel netlink monitor for per-interface carrier detection and route changes.
    // Fires within milliseconds of cable pull, NM disconnect, or route failover.
    netlink::spawn_link_monitor(shared_manager.state_change_sender());

    // Wait for shutdown or reload signal
    let shutdown_manager = shared_manager.clone();
    wait_for_signals(shared_manager).await;

    info!("Shutting down...");

    // Close all TV connections gracefully so the TV drops its SSAP
    // session immediately (avoids "Connection reset" on warm restart).
    shutdown_manager.close_all_connections().await;

    // Abort the signal worker
    signal_worker.abort();

    // Drop connection to unregister from D-Bus.
    // This releases the well-known name, triggering NameOwnerChanged
    // so clients (tray) detect the daemon went away.
    drop(connection);

    Ok(())
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(manager: Arc<TvManager>) {
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
                if let Err(e) = manager.reload_config().await {
                    warn!("Failed to reload config: {}", e);
                }
            }
        }
    }
}
