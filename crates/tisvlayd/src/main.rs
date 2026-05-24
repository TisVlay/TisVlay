//! tisvlayd - TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling TVs.
//! Supports multiple TV vendors through a backend registry system.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{SystemConfig, absolutize};
use tracing::{info, warn};

use tisvlayd::dbus::{
    BusKind, DBUS_NAME, ManagerService, apply_reload_diff, build_dbus_connection,
};
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

    /// Pin the SSDP source UDP port for firewall-friendly discovery.
    ///
    /// Overrides `[discovery] bind_port` in the config file. Useful for
    /// declaratively-managed deployments (NixOS, container images)
    /// that want to set the port at the deployment layer without
    /// making the daemon's writable config read-only.
    /// See docs/firewall.md.
    #[arg(long, value_name = "PORT")]
    bind_port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // `netlink_packet_route` emits a chatty `log::warn!()` on every
    // interface enumeration on newer kernels (IFLA_INET6_CONF size
    // mismatch — harmless kernel/crate version skew). Suppressed at
    // the log→tracing bridge level via `ignore_crate` rather than
    // an EnvFilter directive: the bridged tracing event's target is
    // the literal string "log", not the original module path, so
    // EnvFilter can't match it. See `logging::init` for the full
    // rationale + the bridge install-order constraint.
    tisvlay_core::logging::init(&["tisvlayd=info", "zbus=warn"], &["netlink_packet_route"])?;

    info!("tisvlayd starting...");

    // Determine config file path. Absolutize relative `--config`
    // arguments against the daemon's CWD at startup so the
    // `ConfigPath` D-Bus property is always an absolute path —
    // consumers (GUI's open-folder action, sysadmins, monitoring
    // tools) can't resolve a relative path against our CWD without
    // racing or guessing.
    let config_path = absolutize(args.config.unwrap_or_else(|| {
        if args.session {
            SystemConfig::user_config_path()
                .unwrap_or_else(|_| PathBuf::from("/var/lib/tisvlay/config.toml"))
        } else {
            PathBuf::from("/var/lib/tisvlay/config.toml")
        }
    }));
    info!(path = %config_path.display(), "Using config file");

    // Load configuration
    let mut config = match SystemConfig::load_from(&config_path) {
        Ok(config) => {
            info!(tvs = config.tv.len(), "Loaded configuration");
            config
        }
        Err(e) => {
            warn!("Failed to load config, using defaults: {}", e);
            SystemConfig::default()
        }
    };

    // CLI overrides on top of the loaded config. Bind-port lives here
    // (rather than as a `settings`-rendered TOML key on a Nix store
    // path) so deployment-time firewall configuration doesn't force
    // the config file read-only — the pairing wizard still writes
    // credentials to the same file.
    if let Some(port) = args.bind_port {
        info!(bind_port = port, "CLI --bind-port overrides config");
        config.discovery.bind_port = Some(port);
    }

    // Create backend registry. Discovery settings (SSDP bind port, …)
    // are read by reference here and don't need to outlive
    // registration — the registry copies whatever each backend
    // needs into its own state. The poll interval is read into a
    // Duration here so it survives the move of `config` into
    // TvManager below.
    let registry = Arc::new(create_registry(&config.discovery));
    let discovery_poll_interval = config.discovery.poll_interval();
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
    let manager_service = ManagerService::new(
        shared_manager.clone(),
        registry.clone(),
        discovery_poll_interval,
    );

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
    wait_for_signals(shared_manager, connection.clone()).await;

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
///
/// SIGHUP threads the D-Bus `connection` so the reload can sync the
/// per-TV object server to the new config alongside the in-memory
/// state — symmetric with `Manager1.ReloadConfig`.
async fn wait_for_signals(manager: Arc<TvManager>, connection: zbus::Connection) {
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
                match manager.reload_config().await {
                    Ok(diff) => {
                        apply_reload_diff(&connection, &manager, &diff).await;
                    }
                    Err(e) => {
                        warn!("Failed to reload config: {}", e);
                    }
                }
            }
        }
    }
}
