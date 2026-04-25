//! Background task that forwards backend state-change events to
//! cached D-Bus properties and emits `PropertiesChanged` signals.
//!
//! Lives in the lib (rather than `main.rs`) so it has access to the
//! private cached fields on each `Tv*Service` struct, and so the
//! integration test harness can spawn it on a private test bus.

use std::sync::Arc;

use tisvlay_core::TvState;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};
use zbus::object_server::SignalEmitter;

use crate::dbus::{
    MANAGER_PATH, ManagerService, TvBacklightService, TvDesktopService, TvInputService,
    TvPowerService, TvScreenService, TvService, TvVolumeService, tv_path,
};
use crate::tv_manager::{StateChangeEvent, TvManager};

/// Forward state change events to D-Bus cached properties.
///
/// Updates cached property fields on per-TV interface structs via
/// `InterfaceRef::get_mut()` and emits standard `PropertiesChanged`
/// signals. No custom signals are emitted.
pub async fn state_change_forwarder(
    connection: zbus::Connection,
    mut rx: mpsc::UnboundedReceiver<StateChangeEvent>,
    manager: Arc<TvManager>,
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
            }
            StateChangeEvent::ConnectionChanged {
                ref tv_id,
                connected,
            } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, connected, "Forwarding connection change to D-Bus");

                if let Ok(iface_ref) = object_server.interface::<_, TvService>(&*path).await {
                    let mut iface = iface_ref.get_mut().await;
                    if iface.connected != connected {
                        iface.connected = connected;
                        if let Err(e) = iface.connected_changed(iface_ref.signal_emitter()).await {
                            warn!(error = %e, tv_id = %tv_id, "Failed to emit Connected property change");
                        }
                    }
                }
            }
            StateChangeEvent::OptionChanged {
                ref tv_id,
                ref key,
                ref value,
            } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, key = %key, value = %value, "Forwarding option change to D-Bus");

                if let Ok(iface_ref) = object_server.interface::<_, TvService>(&*path).await {
                    let mut iface = iface_ref.get_mut().await;
                    let emitter = iface_ref.signal_emitter();
                    match key.as_str() {
                        "managed" => {
                            if let Ok(managed) = value.parse::<bool>()
                                && iface.managed != managed
                            {
                                iface.managed = managed;
                                iface.managed_changed(emitter).await.ok();
                            }
                        }
                        "name" if iface.name != *value => {
                            iface.name = value.clone();
                            iface.name_changed(emitter).await.ok();
                        }
                        "profile.system" if iface.system_profile != *value => {
                            iface.system_profile = value.clone();
                            iface.system_profile_changed(emitter).await.ok();
                        }
                        "primary_input" if iface.primary_input != *value => {
                            iface.primary_input = value.clone();
                            iface.primary_input_changed(emitter).await.ok();
                        }
                        _ => {}
                    }
                }
            }
            StateChangeEvent::DesktopMappingChanged { ref tv_id } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, "Forwarding desktop mapping change to D-Bus");
                if let Ok(iface_ref) = object_server.interface::<_, TvDesktopService>(&*path).await
                {
                    let emitter = iface_ref.signal_emitter();
                    let iface = iface_ref.get_mut().await;
                    // `invalidates` property — clients re-read on next access.
                    iface.connector_changed(emitter).await.ok();
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
            StateChangeEvent::InterfaceDown { ref interface } => {
                debug!(interface = %interface, "Interface carrier down: disconnecting affected TVs");
                manager.disconnect_tvs_on_interface(interface).await;
            }
            StateChangeEvent::InterfaceUp { ref interface } => {
                debug!(interface = %interface, "Interface carrier up: reconnecting affected TVs");
                manager.reconnect_tvs_on_interface(interface).await;
            }
            StateChangeEvent::RouteChanged => {
                trace!("Route changed: re-resolving monitored interfaces");
                manager.re_resolve_all_interfaces().await;
                // Coalesce: brief collection window then drain queued
                // RouteChanged events. The resolve above is instant
                // (microseconds), but route events arrive in bursts
                // over seconds — the 100ms pause lets stragglers
                // accumulate so they collapse into a single pass.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let mut coalesced = 0u32;
                loop {
                    match rx.try_recv() {
                        Ok(StateChangeEvent::RouteChanged) => coalesced += 1,
                        Ok(other) => {
                            let _ = manager.state_change_sender().send(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                if coalesced > 0 {
                    trace!(coalesced, "Re-resolving after coalesced route events");
                    manager.re_resolve_all_interfaces().await;
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
async fn update_tv_cached_state(object_server: &zbus::ObjectServer, path: &str, state: &TvState) {
    // TvService: mark connected + sync current_input for Automated computation
    if let Ok(iface_ref) = object_server.interface::<_, TvService>(path).await {
        let mut iface = iface_ref.get_mut().await;
        let emitter = iface_ref.signal_emitter();
        if !iface.connected {
            iface.connected = true;
            iface.connected_changed(emitter).await.ok();
        }
        if let Some(ref input) = state.current_input {
            if iface.current_input != *input {
                iface.current_input = input.clone();
                // Automated depends on current_input, signal it changed
                iface.automated_changed(emitter).await.ok();
            }
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
                .is_screen_on_changed(iface_ref.signal_emitter())
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
