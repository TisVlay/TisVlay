//! tisvlayd - TV Control Daemon
//!
//! A systemd system service that provides D-Bus API for controlling TVs.
//! Supports multiple TV vendors through a backend registry system.

mod dbus;
mod logind;
mod registry;
mod tv_manager;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{SystemConfig, TvState, UserConfig};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::object_server::SignalEmitter;

use crate::dbus::{
    register_tv_interfaces, tv_path, ManagerService, TvBacklightService, TvInputService,
    TvPowerService, TvScreenService, TvService, TvVolumeService, DBUS_NAME, MANAGER_PATH,
    OBJECT_MANAGER_PATH,
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

    // Determine config file path
    let config_path = args.config.unwrap_or_else(|| {
        if args.session {
            UserConfig::config_path()
                .unwrap_or_else(|_| PathBuf::from("/var/lib/tisvlay/config.toml"))
        } else {
            PathBuf::from("/var/lib/tisvlay/config.toml")
        }
    });
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

    // Register ObjectManager at /org/tisvlay (ancestor of all tisvlay objects).
    // Must be registered first so that subsequent at() calls trigger InterfacesAdded.
    connection
        .object_server()
        .at(OBJECT_MANAGER_PATH, zbus::fdo::ObjectManager)
        .await
        .context("Failed to serve ObjectManager interface")?;
    info!(
        "ObjectManager interface registered at {}",
        OBJECT_MANAGER_PATH
    );

    // Create and register manager service at /org/tisvlay/Manager
    let manager_service =
        ManagerService::new(shared_manager.clone(), registry.clone(), connection.clone());
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

    // Execute on_startup action (e.g., power_on wakes TVs via WoL).
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

/// Forward state change events to D-Bus cached properties and signals.
///
/// For subscription updates: updates cached properties on all per-TV interface
/// structs via `InterfaceRef::get_mut()` and emits `PropertiesChanged` for each
/// changed property. Also emits legacy `StateChanged` signal for backward compat.
///
/// For connection changes: updates the `Connected` cached property.
///
/// For other events: emits legacy signals as before.
async fn state_change_forwarder(
    connection: zbus::Connection,
    mut rx: mpsc::UnboundedReceiver<StateChangeEvent>,
) {
    info!("State change forwarder started");
    let object_server = connection.object_server();

    while let Some(event) = rx.recv().await {
        match event {
            StateChangeEvent::SubscriptionUpdate {
                ref tv_id,
                ref state,
            } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, "Forwarding subscription update to D-Bus properties");

                update_tv_cached_state(object_server, &path, state).await;

                // Also emit legacy StateChanged signal (backward compat, removed in Phase C)
                let json = serde_json::to_string(state).unwrap_or_default();
                if let Ok(emitter) = SignalEmitter::new(&connection, path.as_str()) {
                    if let Err(e) = TvService::state_changed(&emitter, &json).await {
                        warn!(error = %e, tv_id = %tv_id, "Failed to emit StateChanged signal");
                    }
                }
            }
            StateChangeEvent::ConnectionChanged {
                ref tv_id,
                connected,
            } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, connected, "Forwarding connection change to D-Bus");

                if let Ok(iface_ref) =
                    object_server.interface::<_, TvService>(&*path).await
                {
                    let mut iface = iface_ref.get_mut().await;
                    if iface.connected != connected {
                        iface.connected = connected;
                        if let Err(e) = iface
                            .connected_changed(iface_ref.signal_emitter())
                            .await
                        {
                            warn!(error = %e, tv_id = %tv_id, "Failed to emit Connected property change");
                        }
                    }
                }

                // Also emit legacy StateChanged signal
                let json = serde_json::json!({ "connected": connected }).to_string();
                if let Ok(emitter) = SignalEmitter::new(&connection, path.as_str()) {
                    TvService::state_changed(&emitter, &json).await.ok();
                }
            }
            StateChangeEvent::TvStateChanged {
                ref tv_id,
                ref state_json,
            } => {
                debug!(tv_id = %tv_id, "Emitting legacy state change signal");

                let path = tv_path(tv_id);
                if let Ok(emitter) = SignalEmitter::new(&connection, path.as_str()) {
                    if let Err(e) = TvService::state_changed(&emitter, state_json).await {
                        warn!(error = %e, tv_id = %tv_id, "Failed to emit StateChanged signal");
                    }
                }
            }
            StateChangeEvent::ConfigReloaded => {
                debug!("Emitting ConfigReloaded signal on Manager1");

                if let Ok(emitter) = SignalEmitter::new(&connection, MANAGER_PATH) {
                    if let Err(e) = ManagerService::config_reloaded(&emitter).await {
                        warn!(error = %e, "Failed to emit ConfigReloaded signal");
                    }
                }
            }
        }
    }

    info!("State change forwarder stopped");
}

/// Update cached D-Bus properties from a backend subscription state snapshot.
///
/// Looks up `InterfaceRef` for each per-TV interface, updates cached fields
/// via `get_mut()`, and emits `PropertiesChanged` only for properties that
/// actually changed. The `_changed` methods are generated by the `#[interface]`
/// macro on the struct and must be called through the `get_mut()` guard.
async fn update_tv_cached_state(
    object_server: &zbus::ObjectServer,
    path: &str,
    state: &TvState,
) {
    // TvService: mark connected (subscription implies connection)
    if let Ok(iface_ref) = object_server.interface::<_, TvService>(path).await {
        let mut iface = iface_ref.get_mut().await;
        if !iface.connected {
            iface.connected = true;
            iface
                .connected_changed(iface_ref.signal_emitter())
                .await
                .ok();
        }
    }

    // TvPowerService: derive power state from booleans
    if let Ok(iface_ref) = object_server.interface::<_, TvPowerService>(path).await {
        let new_power = match (state.is_on, state.is_screen_on) {
            (false, _) => "standby",
            (true, false) => "screen_off",
            (true, true) => "active",
        };
        let mut iface = iface_ref.get_mut().await;
        if iface.power_state != new_power {
            iface.power_state = new_power.to_string();
            iface
                .power_state_changed(iface_ref.signal_emitter())
                .await
                .ok();
        }
    }

    // TvScreenService
    if let Ok(iface_ref) = object_server.interface::<_, TvScreenService>(path).await {
        let mut iface = iface_ref.get_mut().await;
        if iface.is_screen_on != state.is_screen_on {
            iface.is_screen_on = state.is_screen_on;
            iface
                .screen_on_prop_changed(iface_ref.signal_emitter())
                .await
                .ok();
        }
    }

    // TvVolumeService
    if let Ok(iface_ref) = object_server.interface::<_, TvVolumeService>(path).await {
        let mut iface = iface_ref.get_mut().await;
        let emitter = iface_ref.signal_emitter();
        if let Some(vol) = state.volume {
            if iface.volume != vol {
                iface.volume = vol;
                iface.volume_changed(emitter).await.ok();
            }
        }
        if let Some(muted) = state.muted {
            if iface.muted != muted {
                iface.muted = muted;
                iface.muted_changed(emitter).await.ok();
            }
        }
    }

    // TvInputService
    if let Ok(iface_ref) = object_server.interface::<_, TvInputService>(path).await {
        let mut iface = iface_ref.get_mut().await;
        let emitter = iface_ref.signal_emitter();
        if let Some(ref input) = state.current_input {
            if iface.current_input != *input {
                iface.current_input = input.clone();
                iface.current_input_changed(emitter).await.ok();
            }
        }
        if !state.inputs.is_empty() {
            let new_inputs: Vec<(String, String)> = state
                .inputs
                .iter()
                .map(|i| (i.id.clone(), i.label.clone()))
                .collect();
            if iface.available_inputs != new_inputs {
                iface.available_inputs = new_inputs;
                iface.available_inputs_invalidate(emitter).await.ok();
            }
        }
    }

    // TvBacklightService
    if let Ok(iface_ref) = object_server.interface::<_, TvBacklightService>(path).await {
        if let Some(bl) = state.backlight {
            let mut iface = iface_ref.get_mut().await;
            if iface.backlight != bl {
                iface.backlight = bl;
                iface
                    .backlight_changed(iface_ref.signal_emitter())
                    .await
                    .ok();
            }
        }
    }
}

/// Wait for SIGTERM, SIGINT (shutdown) or SIGHUP (reload).
async fn wait_for_signals(manager: Arc<TvManager>) {
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
                if let Err(e) = manager.reload_config().await {
                    warn!("Failed to reload config: {}", e);
                }
            }
        }
    }
}
