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
use crate::desktop_probe;
use crate::tv_manager::{StateChangeEvent, TvManager};

/// Re-run the DRM-EDID probe and update each managed TV's
/// [`tisvlay_core::DesktopMapping`] cache on the manager.
///
/// Cheap: probes `/sys/class/drm/`, parses each connector's EDID via
/// `edidkit`, runs the deterministic matcher (sort by connector name,
/// then greedy claim per TV by `HardwareId`). Triggered on every
/// `ConnectionChanged(true)` event (a freshly-connected backend's
/// `HardwareId` is now available so the global match should
/// re-evaluate) and on `ConfigReloaded` (the TV list may have changed).
///
/// TVs the matcher couldn't resolve get cleared to an empty mapping
/// so `Tv1.Desktop.Connector` reflects "no match" rather than a
/// stale value from a prior probe.
pub(crate) async fn refresh_desktop_mapping(manager: &TvManager) {
    let tv_ids = manager.list_tv_ids().await;

    let mut tvs_for_match: Vec<(String, tisvlay_core::HardwareId)> = Vec::new();
    for tv_id in &tv_ids {
        if let Some(hw) = manager.get_hardware_id(tv_id).await
            && !hw.is_empty()
        {
            tvs_for_match.push((tv_id.clone(), hw));
        }
    }

    let probed = desktop_probe::ProbedDisplay::enumerate();
    debug!(
        count = probed.len(),
        candidates = tvs_for_match.len(),
        "Desktop probe enumerated DRM displays"
    );

    let matches = desktop_probe::match_displays_to_tvs(&probed, &tvs_for_match);

    for tv_id in &tv_ids {
        let mapping = match matches.get(tv_id) {
            Some(d) => tisvlay_core::DesktopMapping {
                connector: d.connector.clone(),
            },
            None => tisvlay_core::DesktopMapping::default(),
        };
        // set_desktop_mapping only emits DesktopMappingChanged when
        // the mapping is *new*; identical re-pushes are no-ops, so
        // calling it for every TV every probe is cheap.
        manager.set_desktop_mapping(tv_id, mapping).await;
    }
}

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

                // The TV's `HardwareId` becomes available after the
                // backend connects (LG fills it from SSAP getSystemInfo
                // on the first command). Re-run the desktop probe now
                // so the EDID matcher has every managed TV's identity
                // populated and the assignment converges to the
                // correct global claim.
                if connected {
                    refresh_desktop_mapping(&manager).await;
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
            StateChangeEvent::PowerCycleProgress { ref tv_id, phase } => {
                let path = tv_path(tv_id);
                debug!(tv_id = %tv_id, phase = %phase, "Forwarding power-cycle phase to D-Bus");
                if let Ok(iface_ref) = object_server.interface::<_, TvPowerService>(&*path).await
                    && let Err(e) = TvPowerService::power_cycle_progress(
                        iface_ref.signal_emitter(),
                        phase.as_str(),
                    )
                    .await
                {
                    warn!(error = %e, tv_id = %tv_id, "Failed to emit PowerCycleProgress");
                }
            }
            StateChangeEvent::PairingProgress {
                ref tv_id,
                ref phase,
                ref message,
            } => {
                debug!(tv_id = %tv_id, phase = %phase, "Forwarding pairing progress to D-Bus");
                if let Ok(emitter) = SignalEmitter::new(&connection, MANAGER_PATH)
                    && let Err(e) =
                        ManagerService::pairing_progress(&emitter, tv_id, phase, message).await
                {
                    warn!(error = %e, tv_id = %tv_id, "Failed to emit PairingProgress");
                }
            }
            StateChangeEvent::ConfigReloaded => {
                debug!("Emitting ConfigReloaded signal on Manager1");

                if let Ok(emitter) = SignalEmitter::new(&connection, MANAGER_PATH) {
                    if let Err(e) = ManagerService::config_reloaded(&emitter).await {
                        warn!(error = %e, "Failed to emit ConfigReloaded signal");
                    }
                }

                // Per-TV config-derived properties (managed, system
                // profile, primary input, automate_when) may have
                // changed via direct config-file edits. The reload
                // path doesn't emit per-key OptionChanged events for
                // these, so resync the cached D-Bus fields here.
                let object_server = connection.object_server();
                for tv_id in manager.list_tv_ids().await {
                    let path = tv_path(&tv_id);
                    refresh_tv_config_state(object_server, &path, &tv_id, &manager).await;
                }

                // The TV list may have changed under us; re-run the
                // global EDID match so any newly-added TVs pick up
                // their connector and any removed TVs drop theirs.
                refresh_desktop_mapping(&manager).await;
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

/// Refresh cached `TvService` properties that derive from the on-disk
/// config (name / managed / system_profile / primary_input /
/// automate_when). Used after `ConfigReloaded` to propagate direct
/// config-file edits — `set_tv_option` already emits per-key
/// `OptionChanged` events, but `reload_config` does not, so without
/// this resync clients see stale cached values until daemon restart.
async fn refresh_tv_config_state(
    object_server: &zbus::ObjectServer,
    path: &str,
    tv_id: &str,
    manager: &Arc<TvManager>,
) {
    let Ok(iface_ref) = object_server.interface::<_, TvService>(path).await else {
        return;
    };

    let info = manager.get_tv_info(tv_id).await;
    let system_profile = manager
        .get_tv_system_profile(tv_id)
        .await
        .unwrap_or_else(|| "default".to_string());
    let primary_input = manager
        .get_tv_primary_input(tv_id)
        .await
        .unwrap_or_default();
    let automate_when = manager
        .get_tv_automate_when(tv_id)
        .await
        .unwrap_or_default();

    let mut iface = iface_ref.get_mut().await;
    let emitter = iface_ref.signal_emitter();

    if let Some(info) = info {
        if iface.name != info.name {
            iface.name = info.name;
            iface.name_changed(emitter).await.ok();
        }
        if iface.managed != info.managed {
            iface.managed = info.managed;
            iface.managed_changed(emitter).await.ok();
        }
    }
    if iface.system_profile != system_profile {
        iface.system_profile = system_profile;
        iface.system_profile_changed(emitter).await.ok();
    }
    if iface.primary_input != primary_input {
        iface.primary_input = primary_input;
        iface.primary_input_changed(emitter).await.ok();
    }
    if iface.automate_when != automate_when {
        iface.automate_when = automate_when;
        iface.automate_when_changed(emitter).await.ok();
        // `Automated` is computed from automate_when + current_input
        // + managed; conservatively re-emit so clients re-read it.
        iface.automated_changed(emitter).await.ok();
    }
}
