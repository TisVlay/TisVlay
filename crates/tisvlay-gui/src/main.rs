//! tisvlay-gui — Slint-based settings + management window.
//!
//! Reads state from `tisvlayd` over the system bus (or session bus
//! with `--user`) and from `tisvlay-session` over the user bus. M2
//! adds writeable controls (volume, backlight, mute, screen toggle,
//! input switcher, profile dropdowns, managed checkbox, remove,
//! reload-config) plus staged power-on feedback via the
//! `Tv1.Power.PowerCycleProgress` signal.

// Slint's `include_modules!()` expands a large block of generated code
// (`InnerAppWindow`, repeater types, etc.) that we don't get to
// annotate. Suppress the workspace lints they trip; they don't apply to
// generated UI scaffolding.
#![allow(unreachable_pub, missing_debug_implementations)]

mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{
    Coalescer, DaemonClient, PowerCyclePhase, ReconcilerMap, RefreshTrigger, SessionClient,
    StateEvent, StateWatcher,
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::state::{StateSnapshot, fetch_state};

slint::include_modules!();

/// Marshal a UI-touching closure onto the Slint event loop. Clones
/// the weak ref internally so callers can pass it by reference; if
/// the `AppWindow` has been dropped by the time the closure runs,
/// it is silently skipped.
fn on_ui<F>(weak: &slint::Weak<AppWindow>, f: F)
where
    F: FnOnce(AppWindow) + Send + 'static,
{
    let weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            f(app);
        }
    })
    .ok();
}

/// Per-(tv_id, property) reconciler maps for ramping TV-state values.
/// User input dispatches through `volume.dispatch` / `backlight.dispatch`
/// before the daemon write; the refresh path observes every fetched
/// value through the same maps and pushes the gated `current()` into
/// the Slint model. Mid-ramp values that don't match the last
/// dispatch are dropped, so the slider thumb stays at user intent
/// during a TV transition. Symmetric with the tray's `Reconcilers`.
#[derive(Clone, Default, Debug)]
struct Reconcilers {
    volume: ReconcilerMap<String, u8>,
    backlight: ReconcilerMap<String, u8>,
}

impl Reconcilers {
    /// Route every TV's volume + backlight in a freshly-fetched
    /// snapshot through the reconcilers' `observe` calls, mutating
    /// the snapshot in place so `push_snapshot` sees gated values.
    /// Called from worker threads BEFORE the
    /// `invoke_from_event_loop` push.
    fn reconcile(&self, snapshot: &mut StateSnapshot) {
        for tv in &mut snapshot.tvs {
            let id = tv.id.to_string();
            tv.volume = self.volume.observe(&id, tv.volume.clamp(0, 100) as u8) as i32;
            tv.backlight = self
                .backlight
                .observe(&id, tv.backlight.clamp(0, 100) as u8) as i32;
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "tisvlay-gui", version, about)]
struct Args {
    /// Use the session bus instead of the system bus (for development
    /// against `tisvlayd --session`).
    #[arg(long)]
    user: bool,
}

fn main() -> Result<()> {
    tisvlay_core::logging::init(&["tisvlay_gui=info", "zbus=warn", "slint=warn"])?;

    let args = Args::parse();
    info!("tisvlay-gui starting");

    // Slint's event loop owns the main thread once `run()` is called.
    // A multi-thread tokio runtime backs all D-Bus work; spawned tasks
    // live on its worker threads. We hold an `_enter` guard on the main
    // thread so any `tokio::spawn` invoked from a Slint callback finds
    // a runtime via `Handle::current()`.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;
    let _enter = rt.enter();

    let app = AppWindow::new()?;

    // Build clients eagerly (synchronously, via block_on) so callbacks
    // can capture them at registration time. This briefly blocks the
    // main thread before `app.run()`, which is fine — these are local
    // bus connection setups, not external I/O.
    let (daemon, session) = rt.block_on(async {
        let daemon = if args.user {
            info!("Connecting to daemon on session bus (development mode)");
            DaemonClient::new_session().await
        } else {
            info!("Connecting to daemon on system bus");
            DaemonClient::new_system().await
        }
        .context("Failed to create daemon client")?;
        let daemon = Arc::new(daemon);

        let session: Arc<RwLock<Option<SessionClient>>> =
            Arc::new(RwLock::new(match SessionClient::new().await {
                Ok(client) if client.is_available().await => Some(client),
                _ => None,
            }));

        anyhow::Ok((daemon, session))
    })?;

    // Per-(tv_id, property) reconcilers shared across the user-input
    // dispatch path (Slint callback → coalescer.apply → reconciler
    // .dispatch → daemon write) and the daemon-refresh path
    // (state watcher → fetch → reconciler.observe → Slint model).
    let reconcilers = Reconcilers::default();

    // Wire callbacks before the event loop starts so they're ready when
    // the user interacts with controls.
    register_callbacks(&app, daemon.clone(), session.clone(), reconcilers.clone());

    // Background watcher: refresh on every signal-driven state change.
    let app_weak = app.as_weak();
    tokio::spawn(initial_fetch_and_watch(
        daemon,
        session,
        reconcilers,
        app_weak,
    ));

    // Slint event loop blocks the main thread; tokio workers keep
    // driving tasks. `rt` drops at end of `main`, aborting any
    // remaining tasks.
    app.run()?;

    info!("tisvlay-gui shutting down");
    Ok(())
}

fn register_callbacks(
    app: &AppWindow,
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
) {
    // Slint global handles. Bound once for the registration phase;
    // closures captured by `move` re-acquire from the upgraded
    // `AppWindow` (the global proxy borrows from the window and
    // can't outlive the function scope).
    let daemon_global = app.global::<Daemon>();
    let wizard = app.global::<PairingWizard>();

    // Slider drags coalesce per TV via `Coalescer`: each `changed(v)`
    // tick fires into the per-TV worker, which drains-keep-latest
    // before each daemon round-trip. Bounds in-flight calls to one
    // per (tv_id, property) regardless of drag rate. Each apply
    // dispatches through the reconciler BEFORE the daemon write so
    // mid-ramp echoes arriving during the round-trip are correctly
    // gated.
    let volume_coalescer: Coalescer<String, u8> = Coalescer::new(|_, new| new, {
        let d = daemon.clone();
        let recs = reconcilers.clone();
        move |id: String, v: u8| {
            let d = d.clone();
            let recs = recs.clone();
            async move {
                recs.volume.dispatch(&id, v);
                if let Err(e) = d.set_volume(&id, v).await {
                    warn!(tv_id = %id, error = %e, "set_volume failed");
                }
            }
        }
    });
    let backlight_coalescer: Coalescer<String, u8> = Coalescer::new(|_, new| new, {
        let d = daemon.clone();
        let recs = reconcilers.clone();
        move |id: String, v: u8| {
            let d = d.clone();
            let recs = recs.clone();
            async move {
                recs.backlight.dispatch(&id, v);
                if let Err(e) = d.set_backlight(&id, v).await {
                    warn!(tv_id = %id, error = %e, "set_backlight failed");
                }
            }
        }
    });

    let coalescer = volume_coalescer.clone();
    daemon_global.on_set_volume(move |tv_id, value| {
        coalescer.fire(tv_id.to_string(), value.clamp(0, 100) as u8);
    });

    let d = daemon.clone();
    daemon_global.on_set_mute(move |tv_id, muted| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_mute(&tv_id, muted).await {
                warn!(tv_id = %tv_id, error = %e, "set_mute failed");
            }
        });
    });

    let coalescer = backlight_coalescer.clone();
    daemon_global.on_set_backlight(move |tv_id, value| {
        coalescer.fire(tv_id.to_string(), value.clamp(0, 100) as u8);
    });

    let d = daemon.clone();
    daemon_global.on_set_screen(move |tv_id, on| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            let result = if on {
                d.screen_on(&tv_id).await
            } else {
                d.screen_off(&tv_id).await
            };
            if let Err(e) = result {
                warn!(tv_id = %tv_id, on, error = %e, "set_is_screen_on failed");
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_switch_input(move |tv_id, input_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let input_id = input_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.switch_input(&tv_id, &input_id).await {
                warn!(tv_id = %tv_id, input = %input_id, error = %e, "switch_input failed");
            }
        });
    });

    // Power-on subscribes to PowerCycleProgress for the duration of
    // the call so the GUI can show staged feedback. External wakes
    // (CLI, tray, other GUI) don't surface their progress here —
    // they'll reflect via the regular Connected state refresh.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_power_on(move |tv_id| {
        let d = d.clone();
        let app_weak = app_weak.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = run_power_on_with_progress(d, app_weak, tv_id.clone()).await {
                warn!(tv_id = %tv_id, error = %e, "power_on failed");
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_power_off(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.power_off(&tv_id).await {
                warn!(tv_id = %tv_id, error = %e, "power_off failed");
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_set_managed(move |tv_id, managed| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            let value = if managed { "true" } else { "false" };
            if let Err(e) = d.set_tv_option(&tv_id, "managed", value).await {
                warn!(tv_id = %tv_id, managed, error = %e, "set_managed failed");
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_set_system_profile(move |tv_id, profile| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_system_profile(&tv_id, &profile).await {
                warn!(tv_id = %tv_id, profile = %profile, error = %e, "set_system_profile failed");
            }
        });
    });

    let s = session.clone();
    daemon_global.on_set_user_profile(move |tv_id, profile| {
        let s = s.clone();
        let tv_id = tv_id.to_string();
        let profile = profile.to_string();
        tokio::spawn(async move {
            let session_ref = s.read().await;
            match session_ref.as_ref() {
                Some(client) => {
                    if let Err(e) = client.set_tv_user_profile(&tv_id, &profile).await {
                        warn!(tv_id = %tv_id, profile = %profile, error = %e, "set_user_profile failed");
                    }
                }
                None => {
                    warn!(tv_id = %tv_id, "set_user_profile: session service unavailable");
                }
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_set_primary_input(move |tv_id, input| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        let input = input.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.set_tv_option(&tv_id, "primary_input", &input).await {
                warn!(tv_id = %tv_id, input = %input, error = %e, "set_primary_input failed");
            }
        });
    });

    let d = daemon.clone();
    daemon_global.on_remove_tv(move |tv_id| {
        let d = d.clone();
        let tv_id = tv_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = d.remove_tv(&tv_id).await {
                warn!(tv_id = %tv_id, error = %e, "remove_tv failed");
            }
        });
    });

    let d = daemon.clone();
    let s = session.clone();
    daemon_global.on_reload_config(move || {
        let d = d.clone();
        let s = s.clone();
        tokio::spawn(async move {
            if let Err(e) = d.reload_config().await {
                warn!(error = %e, "daemon reload_config failed");
            }
            let session_ref = s.read().await;
            if let Some(client) = session_ref.as_ref()
                && let Err(e) = client.reload_config().await
            {
                warn!(error = %e, "session reload_config failed");
            }
        });
    });

    // -------------------------------------------------------------------------
    // Pairing wizard
    // -------------------------------------------------------------------------

    // Open wizard + start discovery.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    daemon_global.on_request_pair(move || {
        let d = d.clone();
        let app_weak = app_weak.clone();
        // Reset wizard state on the Slint side first so the user sees
        // the wizard immediately, then kick off discovery.
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            reset_wizard(&app);
            wizard.set_active(true);
            wizard.set_step(1);
            wizard.set_discovery_streaming(true);
        }
        tokio::spawn(async move {
            run_wizard_discovery(d, app_weak).await;
        });
    });

    let app_weak = app.as_weak();
    wizard.on_cancel(move || {
        info!("wizard: cancel fired");
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(&app);
        }
    });

    // Step transitions. Driven entirely from Slint property reads.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    wizard.on_step_next(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let step = wizard.get_step();
        info!(step, "wizard: step-next fired");
        match step {
            1 => {
                // Discover → Configure: copy selected TV's vendor /
                // address / model and a default name into the
                // wizard's AppWindow-level fields, then advance.
                let i = wizard.get_selected_index();
                if i < 0 {
                    return;
                }
                let model = wizard.get_discovered();
                if let Some(tv) = model.row_data(i as usize) {
                    if wizard.get_tv_name().is_empty() {
                        let default = if tv.name.is_empty() {
                            tv.address.clone()
                        } else {
                            tv.name.clone()
                        };
                        wizard.set_tv_name(default);
                    }
                    wizard.set_selected_vendor(tv.vendor);
                    wizard.set_selected_address(tv.address);
                    wizard.set_selected_hw_model(tv.hw_model);
                }
                if wizard.get_system_profile().is_empty() {
                    wizard.set_system_profile("default".into());
                }
                wizard.set_step(2);
            }
            2 => {
                // Configure → Pair: kick off pair_tv with collected fields.
                let name = wizard.get_tv_name().to_string();
                if name.is_empty() {
                    return;
                }
                let i = wizard.get_selected_index();
                if i < 0 {
                    return;
                }
                let model = wizard.get_discovered();
                let Some(tv) = model.row_data(i as usize) else {
                    return;
                };
                let vendor = tv.vendor.to_string();
                let address = tv.address.to_string();
                let mac = wizard.get_manual_mac().to_string();
                let wake_id = if mac.is_empty() { None } else { Some(mac) };
                let managed = wizard.get_managed();

                wizard.set_step(3);
                wizard.set_pair_phase(SharedString::default());
                wizard.set_pair_message(SharedString::default());

                let d = d.clone();
                let app_weak = app.as_weak();
                tokio::spawn(async move {
                    run_wizard_pair(d, app_weak, name, vendor, address, wake_id, managed).await;
                });
            }
            // Pair → Done. Only allowed when pair_phase == "complete".
            3 if wizard.get_pair_phase() == "complete" => {
                wizard.set_step(4);
            }
            _ => {}
        }
    });

    let app_weak = app.as_weak();
    wizard.on_step_back(move || {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            let step = wizard.get_step();
            if step > 1 {
                wizard.set_step(step - 1);
            }
        }
    });

    // Pair-failed → discovery: clear pair state and rewind to step 1.
    let app_weak = app.as_weak();
    wizard.on_back_to_discovery(move || {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            wizard.set_pair_phase(SharedString::default());
            wizard.set_pair_message(SharedString::default());
            wizard.set_step(1);
        }
    });

    // Pair-failed → retry: restart pair_tv with the same parameters.
    let d = daemon.clone();
    let app_weak = app.as_weak();
    wizard.on_retry_pair(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let name = wizard.get_tv_name().to_string();
        let i = wizard.get_selected_index();
        if i < 0 || name.is_empty() {
            return;
        }
        let model = wizard.get_discovered();
        let Some(tv) = model.row_data(i as usize) else {
            return;
        };
        let vendor = tv.vendor.to_string();
        let address = tv.address.to_string();
        let mac = wizard.get_manual_mac().to_string();
        let wake_id = if mac.is_empty() { None } else { Some(mac) };
        let managed = wizard.get_managed();

        wizard.set_pair_phase(SharedString::default());
        wizard.set_pair_message(SharedString::default());

        let d = d.clone();
        let app_weak = app.as_weak();
        tokio::spawn(async move {
            run_wizard_pair(d, app_weak, name, vendor, address, wake_id, managed).await;
        });
    });

    let app_weak = app.as_weak();
    wizard.on_finish(move || {
        if let Some(app) = app_weak.upgrade() {
            reset_wizard(&app);
        }
    });

    // Selection changed: copy the selected discovered TV's vendor /
    // address / hw_model to the AppWindow-level selected-* properties
    // so step 2 / step 3 / step 4 read consistent values without
    // round-tripping through model row indexing.
    let app_weak = app.as_weak();
    wizard.on_selected_changed(move |i| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        wizard.set_selected_index(i);
        if i < 0 {
            wizard.set_selected_vendor(SharedString::default());
            wizard.set_selected_address(SharedString::default());
            wizard.set_selected_hw_model(SharedString::default());
            return;
        }
        let model = wizard.get_discovered();
        if let Some(tv) = model.row_data(i as usize) {
            wizard.set_selected_vendor(tv.vendor);
            wizard.set_selected_address(tv.address);
            wizard.set_selected_hw_model(tv.hw_model);
        }
    });

    // Other input handlers — Slint's `<=>` already handles
    // property-level sync; these are no-ops for now.
    wizard.on_name_edited(|_| {});
    wizard.on_profile_selected(|_| {});
    wizard.on_managed_toggled(|_| {});

    // Manual-entry: edits flush back via two-way binding; the explicit
    // edited callbacks are wire-only no-ops to satisfy LineEdit.
    // The vendor ComboBox emits the *display name* (e.g. "LG WebOS").
    // Resolve back to the canonical vendor id by looking up in
    // `wizard_supported_vendors`.
    let app_weak = app.as_weak();
    wizard.on_manual_vendor_edited(move |display_name| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let model = wizard.get_supported_vendors();
        for i in 0..model.row_count() {
            if let Some(opt) = model.row_data(i)
                && opt.name == display_name
            {
                wizard.set_manual_vendor(opt.id);
                return;
            }
        }
        // No match — accept the display name as-is so a future
        // backend doesn't get silently dropped.
        wizard.set_manual_vendor(display_name);
    });
    let app_weak = app.as_weak();
    wizard.on_manual_ip_edited(move |v| {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            wizard.set_manual_ip(v);
        }
    });
    let app_weak = app.as_weak();
    wizard.on_manual_mac_edited(move |v| {
        if let Some(app) = app_weak.upgrade() {
            let wizard = app.global::<PairingWizard>();
            wizard.set_manual_mac(v);
        }
    });

    // Manual-add: synthesize a discovered-TV row from the user's
    // typed vendor + IP, append it to the wizard model, select it,
    // and advance to Configure. From step 2 on the flow is identical
    // to the SSDP path.
    let app_weak = app.as_weak();
    wizard.on_manual_add(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let wizard = app.global::<PairingWizard>();
        let mut vendor = wizard.get_manual_vendor().to_string();
        if vendor.is_empty() {
            // Slint property defaults can drop through `<=>` chains in
            // some configurations — fall back to LG since that's the
            // only vendor the manual UI currently offers.
            vendor = "lg".to_string();
        }
        let ip = wizard.get_manual_ip().to_string();
        if ip.is_empty() {
            return;
        }
        info!(vendor = %vendor, ip = %ip, "wizard: manual add");

        let synthetic = WizardDiscoveredTv {
            id: SharedString::default(),
            name: SharedString::default(),
            vendor: SharedString::from(vendor.clone()),
            address: SharedString::from(ip.clone()),
            hw_model: SharedString::default(),
        };

        // Append to existing list (or create a fresh one) so any
        // already-discovered TVs aren't lost.
        let model = wizard.get_discovered();
        let mut rows: Vec<WizardDiscoveredTv> = (0..model.row_count())
            .filter_map(|i| model.row_data(i))
            .collect();
        rows.push(synthetic);
        let new_index = (rows.len() - 1) as i32;
        wizard.set_discovered(ModelRc::new(VecModel::from(rows)));
        wizard.set_selected_index(new_index);
        // Also publish individual selected-* fields so the wizard
        // doesn't have to round-trip through model row indexing for
        // step 2's vendor / address display.
        wizard.set_selected_vendor(SharedString::from(vendor));
        wizard.set_selected_address(SharedString::from(ip));
        wizard.set_selected_hw_model(SharedString::default());
        if wizard.get_system_profile().is_empty() {
            wizard.set_system_profile("default".into());
        }
        wizard.set_step(2);
    });
}

/// Clear all wizard state and close the wizard.
fn reset_wizard(app: &AppWindow) {
    let wizard = app.global::<PairingWizard>();
    wizard.set_active(false);
    wizard.set_step(1);
    wizard.set_selected_index(-1);
    wizard.set_tv_name(SharedString::default());
    wizard.set_system_profile(SharedString::default());
    wizard.set_managed(true);
    wizard.set_manual_vendor("lg".into());
    wizard.set_manual_ip(SharedString::default());
    wizard.set_manual_mac(SharedString::default());
    wizard.set_selected_vendor(SharedString::default());
    wizard.set_selected_address(SharedString::default());
    wizard.set_selected_hw_model(SharedString::default());
    wizard.set_discovery_streaming(false);
    wizard.set_discovery_error(SharedString::default());
    wizard.set_pair_phase(SharedString::default());
    wizard.set_pair_message(SharedString::default());
    wizard.set_discovered(ModelRc::new(VecModel::<WizardDiscoveredTv>::default()));
}

/// Drive a single discovery scan and stream results into the wizard's
/// `wizard_discovered` model. Subscribes to `Manager1.DiscoveryFound`
/// before invoking `discover_tvs` so signals emitted during the call
/// are captured. v1: the daemon emits in a burst right before the
/// RPC returns, but the subscription pattern is forward-compatible.
async fn run_wizard_discovery(daemon: Arc<DaemonClient>, app_weak: slint::Weak<AppWindow>) {
    // v1: the daemon currently emits DiscoveryFound signals in a burst
    // right before discover_tvs returns, so subscribing here adds no
    // streaming benefit yet — we just take the synchronous RPC return.
    // When the daemon refactors to per-backend streaming, swap this
    // for a discovery_found_stream() drainer (the helper exists on
    // DaemonClient; the wire shape is forward-compatible).
    match daemon.discover_tvs(5000).await {
        Ok(tvs) => {
            let rows: Vec<WizardDiscoveredTv> = tvs
                .into_iter()
                .map(|(id, name, address, hw_id, vendor)| WizardDiscoveredTv {
                    id: SharedString::from(id),
                    name: SharedString::from(name),
                    vendor: SharedString::from(vendor),
                    address: SharedString::from(address),
                    hw_model: SharedString::from(hw_id.model.unwrap_or_default()),
                })
                .collect();
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovered(ModelRc::new(VecModel::from(rows)));
                wizard.set_discovery_streaming(false);
            });
        }
        Err(e) => {
            let msg = format!("{e}");
            on_ui(&app_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_discovery_streaming(false);
                wizard.set_discovery_error(SharedString::from(msg));
            });
        }
    }
}

/// Build vendor-agnostic config JSON for pairing via daemon.
///
/// Matches the CLI's `build_pairing_config_json` shape: each vendor
/// connector accepts `address` and `wake_id` via serde aliases
/// alongside its native field names. Empty `wake_id` → field omitted.
fn build_pairing_config_json(address: &str, wake_id: Option<&str>) -> String {
    let mut config = serde_json::json!({ "address": address });
    if let Some(wid) = wake_id.filter(|s| !s.is_empty()) {
        config["wake_id"] = serde_json::Value::String(wid.to_string());
    }
    config.to_string()
}

/// Run a `pair_tv` call with live progress updates streamed into the
/// wizard's `wizard_pair_phase` / `wizard_pair_message` fields.
/// Mirrors `run_power_on_with_progress` but for the pairing flow.
async fn run_wizard_pair(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    address: String,
    wake_id: Option<String>,
    managed: bool,
) {
    use futures_util::StreamExt;

    // Subscribe before issuing the call so we don't miss the first
    // phase (typically `awaiting_tv` on LG).
    let stream = match daemon.pairing_progress_stream().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "PairingProgress subscribe failed; falling back to phase-less pair");
            // Fall through and just call pair_tv without progress updates.
            run_pair_no_progress(daemon, app_weak, name, vendor, address, wake_id, managed).await;
            return;
        }
    };

    let drainer_weak = app_weak.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some((_tv_id, phase, message)) = s.next().await {
            on_ui(&drainer_weak, move |app| {
                let wizard = app.global::<PairingWizard>();
                wizard.set_pair_phase(SharedString::from(phase));
                wizard.set_pair_message(SharedString::from(message));
            });
        }
    });

    let config_json = build_pairing_config_json(&address, wake_id.as_deref());
    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    drainer.abort();

    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        match result {
            Ok(_tv_id) => {
                wizard.set_pair_phase("complete".into());
                if wizard.get_pair_message().is_empty() {
                    wizard.set_pair_message("Client key stored.".into());
                }
            }
            Err(e) => {
                wizard.set_pair_phase("failed".into());
                wizard.set_pair_message(SharedString::from(format!("{e}")));
            }
        }
    });
}

/// Fallback pair flow when the PairingProgress subscription fails to
/// set up. Just calls pair_tv synchronously and reflects the final
/// success/failure outcome in the wizard.
async fn run_pair_no_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    name: String,
    vendor: String,
    address: String,
    wake_id: Option<String>,
    managed: bool,
) {
    let config_json = build_pairing_config_json(&address, wake_id.as_deref());
    let result = daemon.pair_tv(&name, &vendor, &config_json, managed).await;
    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        match result {
            Ok(_) => {
                wizard.set_pair_phase("complete".into());
                wizard.set_pair_message("Paired successfully.".into());
            }
            Err(e) => {
                wizard.set_pair_phase("failed".into());
                wizard.set_pair_message(SharedString::from(format!("{e}")));
            }
        }
    });
}

/// Subscribe to PowerCycleProgress for `tv_id`, fire `power_on`, and
/// stream phase strings into the matching row's `power_cycle_phase`
/// field. Drains the subscription for the duration of the call.
async fn run_power_on_with_progress(
    daemon: Arc<DaemonClient>,
    app_weak: slint::Weak<AppWindow>,
    tv_id: String,
) -> Result<()> {
    use futures_util::StreamExt;

    let stream = daemon.power_cycle_progress_stream(&tv_id).await?;
    let app_weak_drainer = app_weak.clone();
    let tv_id_drainer = tv_id.clone();
    let drainer = tokio::spawn(async move {
        let mut s = std::pin::pin!(stream);
        while let Some(phase) = s.next().await {
            update_power_cycle_phase(&app_weak_drainer, &tv_id_drainer, Some(phase));
        }
    });

    let result = daemon.power_on(&tv_id).await;
    drainer.abort();

    // Clear the phase indicator so the button re-enables. The regular
    // refresh will repopulate `connected: true` (or surface the
    // failure via daemon state changes).
    update_power_cycle_phase(&app_weak, &tv_id, None);

    result
}

/// Update a single TV's `power_cycle_phase` field on the Slint side.
/// `None` clears the phase (button returns to idle); `Some(phase)`
/// renders the staged label.
fn update_power_cycle_phase(
    app_weak: &slint::Weak<AppWindow>,
    tv_id: &str,
    phase: Option<PowerCyclePhase>,
) {
    let tv_id = tv_id.to_string();
    on_ui(app_weak, move |app| {
        let model = app.get_tvs();
        for i in 0..model.row_count() {
            let Some(mut row) = model.row_data(i) else {
                continue;
            };
            if row.id != tv_id {
                continue;
            }
            row.power_cycle_phase = match phase {
                Some(p) => SharedString::from(p.as_str()),
                None => SharedString::default(),
            };
            model.set_row_data(i, row);
            break;
        }
    });
}

async fn initial_fetch_and_watch(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
) {
    let initial_session_available = session.read().await.is_some();
    let initial_snapshot = match fetch_state(&daemon, session.read().await.as_ref()).await {
        Ok(mut s) => {
            reconcilers.reconcile(&mut s);
            Some(s)
        }
        Err(e) => {
            debug!(error = %e, "Initial state fetch failed; daemon may be offline");
            None
        }
    };

    // Fetch the daemon's vendor list once at startup. Used by the
    // pairing wizard's manual-entry vendor selector. Failures are
    // tolerable — the wizard falls back to its hardcoded "lg" default.
    let vendor_pairs = daemon.get_supported_vendors().await.unwrap_or_default();
    debug!(
        count = vendor_pairs.len(),
        "Fetched supported vendors from daemon"
    );

    let initial_daemon_available = initial_snapshot.is_some();
    on_ui(&app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        if let Some(snapshot) = initial_snapshot {
            push_snapshot(&app, snapshot);
        }
        let names: Vec<SharedString> = vendor_pairs
            .iter()
            .map(|(_, name)| SharedString::from(name.as_str()))
            .collect();
        let vendors: Vec<VendorOption> = vendor_pairs
            .into_iter()
            .map(|(id, name)| VendorOption {
                id: SharedString::from(id),
                name: SharedString::from(name),
            })
            .collect();
        wizard.set_supported_vendors(ModelRc::new(VecModel::from(vendors)));
        wizard.set_supported_vendor_names(ModelRc::new(VecModel::from(names)));
        app.set_daemon_available(initial_daemon_available);
        app.set_session_available(initial_session_available);
    });

    watch_state(daemon, session, reconcilers, app_weak).await;
}

async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
) {
    // No debounce: every D-Bus state signal triggers a refresh fire.
    // The default 100ms (originally a flood guard before
    // `RefreshTrigger` existed) is now redundant — `RefreshTrigger`
    // already folds concurrent fires into a single in-flight refresh
    // plus one trailing-edge follow-up, so floods can't backlog.
    //
    // Removing the debounce means the daemon→GUI sample rate is bounded
    // by `fetch_state` latency (≈ms locally), not by a fixed timer.
    // Slint's winit backend then rate-limits the visual side: it
    // queries the live monitor refresh rate via
    // `current_monitor().refresh_rate_millihertz()` and coalesces
    // dirty-property updates between vsync frames (verified in
    // `internal/backends/winit/frame_throttle.rs`). So we get whatever
    // the display actually supports — 60 Hz, 120 Hz, VRR, mixed
    // multi-monitor — without hardcoding a sample rate. Spamming
    // `invoke_from_event_loop` is cheap; the property tracker dedups
    // and only one paint per frame happens regardless of how many
    // updates feed in.
    let watcher_config = tisvlay_core::StateWatcherConfig {
        signal_debounce: std::time::Duration::ZERO,
        ..Default::default()
    };
    let mut watcher = match StateWatcher::new(&daemon, watcher_config).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher; live updates disabled");
            return;
        }
    };

    // `watcher.next() → refresh.await` inline halts signal_stream
    // draining and wedges the connection under flood. See
    // `RefreshTrigger`'s docstring for the bug history.
    let trigger = RefreshTrigger::new();
    let trigger_refresh = || {
        let daemon = daemon.clone();
        let session = session.clone();
        let reconcilers = reconcilers.clone();
        let app_weak = app_weak.clone();
        trigger.fire(move || {
            let daemon = daemon.clone();
            let session = session.clone();
            let reconcilers = reconcilers.clone();
            let app_weak = app_weak.clone();
            async move { refresh(&daemon, &session, &reconcilers, &app_weak).await }
        });
    };

    loop {
        let Some(event) = watcher.next().await else {
            break;
        };

        match event {
            StateEvent::DaemonStateChanged(_)
            | StateEvent::DaemonAppeared
            | StateEvent::SessionProfileChanged
            | StateEvent::FallbackPoll => {
                trigger_refresh();
            }
            StateEvent::DaemonDisappeared => {
                on_ui(&app_weak, move |app| {
                    app.set_daemon_available(false);
                    app.set_tvs(ModelRc::new(VecModel::<TvViewModel>::default()));
                });
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await
                    && client.is_available().await
                {
                    *session.write().await = Some(client);
                    on_ui(&app_weak, move |app| {
                        app.set_session_available(true);
                    });
                    trigger_refresh();
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                on_ui(&app_weak, move |app| {
                    app.set_session_available(false);
                });
                trigger_refresh();
            }
        }
    }
}

async fn refresh(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    reconcilers: &Reconcilers,
    app_weak: &slint::Weak<AppWindow>,
) {
    let session_ref = session.read().await;
    match fetch_state(daemon, session_ref.as_ref()).await {
        Ok(mut snapshot) => {
            reconcilers.reconcile(&mut snapshot);
            on_ui(app_weak, move |app| {
                push_snapshot(&app, snapshot);
                app.set_daemon_available(true);
            });
        }
        Err(e) => {
            debug!(error = %e, "State refresh failed");
            on_ui(app_weak, move |app| {
                app.set_daemon_available(false);
            });
        }
    }
}

/// Wrap a fetched [`StateSnapshot`] into Slint models and push to the
/// AppWindow. Must run on the Slint event-loop thread (it builds
/// `ModelRc` instances which are `!Send`).
fn push_snapshot(app: &AppWindow, snapshot: StateSnapshot) {
    let tvs: Vec<TvViewModel> = snapshot
        .tvs
        .into_iter()
        .map(|raw| TvViewModel {
            id: raw.id,
            name: raw.name,
            vendor: raw.vendor,
            ip: raw.ip,
            connected: raw.connected,
            power_state: raw.power_state,
            is_screen_on: raw.is_screen_on,
            volume: raw.volume,
            muted: raw.muted,
            backlight: raw.backlight,
            current_input: raw.current_input,
            available_input_ids: ModelRc::new(VecModel::from(raw.available_input_ids)),
            system_profile: raw.system_profile,
            user_profile: raw.user_profile,
            primary_input: raw.primary_input,
            managed: raw.managed,
            automated: raw.automated,
            hw_manufacturer: raw.hw_manufacturer,
            hw_model: raw.hw_model,
            hw_serial: raw.hw_serial,
            desktop_connector: raw.desktop_connector,
            // Phase is purely GUI-side state. Refreshes reset it; any
            // concurrent in-flight power-on task overwrites later.
            power_cycle_phase: SharedString::default(),
        })
        .collect();
    sync_vec_model(&app.get_tvs(), tvs, |m| app.set_tvs(m));
    sync_vec_model(&app.get_system_profiles(), snapshot.system_profiles, |m| {
        app.set_system_profiles(m)
    });
    sync_vec_model(&app.get_user_profiles(), snapshot.user_profiles, |m| {
        app.set_user_profiles(m)
    });
}

/// Replace a `ModelRc` content **in place** when it's backed by a
/// `VecModel`, falling back to a full handle replacement otherwise
/// (typically only on first push, or after a model reset).
///
/// In-place mutation is essential for any model whose `for ... in
/// model` repeater hosts widgets with internal state — Slider drags,
/// ScrollView positions, focus, hover. Replacing the `ModelRc` handle
/// triggers Slint's repeater to treat it as a model reset and rebuild
/// every child from scratch, which detaches mouse capture mid-drag and
/// resets scroll offsets.
///
/// This helper diffs by index: `set_row_data` for matching rows, push
/// for new rows past the existing length, remove for trailing rows
/// that no longer exist. The repeater fires `row_changed` /
/// `row_added` / `row_removed` signals — never a model reset — so
/// child widgets survive across pushes.
fn sync_vec_model<T: Clone + 'static>(
    current: &ModelRc<T>,
    new_rows: Vec<T>,
    fallback: impl FnOnce(ModelRc<T>),
) {
    let Some(vec_model) = current.as_any().downcast_ref::<VecModel<T>>() else {
        fallback(ModelRc::new(VecModel::from(new_rows)));
        return;
    };
    let cur_len = vec_model.row_count();
    let new_len = new_rows.len();
    for (i, row) in new_rows.into_iter().enumerate() {
        if i < cur_len {
            vec_model.set_row_data(i, row);
        } else {
            vec_model.push(row);
        }
    }
    while vec_model.row_count() > new_len {
        vec_model.remove(vec_model.row_count() - 1);
    }
}
