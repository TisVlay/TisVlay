//! Long-running state-watch loop. Drives the bus-signal subscription
//! that feeds GUI refreshes, applies value-ramping reconcilers, and
//! pushes view-model snapshots into the AppWindow's models.

use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{
    DaemonClient, RefreshTrigger, SessionClient, StateEvent, StateWatcher, TrayClient,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::about::{PeerInfo, SupportedVendorsCache, make_about_components, probe_cli_version};
use crate::state::{StateSnapshot, fetch_state};
use crate::{
    AppWindow, PairingWizard, Reconcilers, TvViewModel, VendorInfoRow, VendorOption, on_ui,
};

/// Boot the state-watch loop. One-shot startup: do the initial state
/// fetch, push the first snapshot, prime the supported-vendors cache,
/// then hand off to [`watch_state`] for the long-running signal-driven
/// refresh path.
pub(crate) async fn initial_fetch_and_watch(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
    supported_vendors_cache: SupportedVendorsCache,
) {
    let initial_session_available = session.read().await.is_some();
    let initial_tray_available = tray.read().await.is_some();
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
    // pairing wizard's manual-entry vendor selector and cached for
    // the diagnostic blob. Failures are tolerable — the wizard falls
    // back to its hardcoded "lg" default; the diagnostic blob renders
    // an empty `vendors_supported` array.
    let mut vendor_pairs = daemon.get_supported_vendors().await.unwrap_or_default();
    // Sort by display name (case-insensitive) so the wizard's vendor
    // ComboBox renders in stable, scannable order regardless of which
    // order the daemon's registry happens to return them in.
    vendor_pairs.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    debug!(
        count = vendor_pairs.len(),
        "Fetched supported vendors from daemon"
    );
    if let Ok(mut cache) = supported_vendors_cache.write() {
        *cache = vendor_pairs.iter().map(|(id, _)| id.clone()).collect();
    }

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
        // Seed `manual-vendor` (and its parallel display name) to the
        // first registered backend so the wizard's vendor ComboBox
        // renders with a sensible default. Only seed if unset —
        // preserve any vendor the user already picked.
        if wizard.get_manual_vendor().is_empty()
            && let Some(first) = vendors.first()
        {
            wizard.set_manual_vendor(first.id.clone());
            wizard.set_manual_vendor_name(first.name.clone());
        }
        wizard.set_supported_vendors(ModelRc::new(VecModel::from(vendors)));
        wizard.set_supported_vendor_names(ModelRc::new(VecModel::from(names)));
        app.set_daemon_available(initial_daemon_available);
        app.set_session_available(initial_session_available);
        app.set_tray_available(initial_tray_available);
    });

    refresh_about_components(&daemon, &session, &tray, &app_weak).await;
    watch_state(daemon, session, tray, reconcilers, app_weak).await;
}

async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
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
        let tray = tray.clone();
        let reconcilers = reconcilers.clone();
        let app_weak = app_weak.clone();
        trigger.fire(move || {
            let daemon = daemon.clone();
            let session = session.clone();
            let tray = tray.clone();
            let reconcilers = reconcilers.clone();
            let app_weak = app_weak.clone();
            async move { refresh(&daemon, &session, &tray, &reconcilers, &app_weak).await }
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
                refresh_about_components(&daemon, &session, &tray, &app_weak).await;
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
            StateEvent::TrayAppeared => {
                if let Ok(client) = TrayClient::new().await
                    && client.is_available().await
                {
                    *tray.write().await = Some(client);
                    on_ui(&app_weak, move |app| {
                        app.set_tray_available(true);
                    });
                    trigger_refresh();
                }
            }
            StateEvent::TrayDisappeared => {
                *tray.write().await = None;
                on_ui(&app_weak, move |app| {
                    app.set_tray_available(false);
                });
                trigger_refresh();
            }
        }
    }
}

async fn refresh(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    tray: &RwLock<Option<TrayClient>>,
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
    drop(session_ref);
    refresh_about_components(daemon, session, tray, app_weak).await;
}

/// Probe each peer's `Version` + `ConfigPath` D-Bus properties and
/// push the COMPONENTS-table model to the About pane. Both
/// properties are `const` upstream, so this is cheap to invoke after
/// every state-affecting event; caching would save a couple of zbus
/// round-trips on a flap-storm but isn't worth the extra plumbing.
async fn refresh_about_components(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    tray: &RwLock<Option<TrayClient>>,
    app_weak: &slint::Weak<AppWindow>,
) {
    async fn probe<F1, F2>(version: F1, config_path: F2) -> Option<PeerInfo>
    where
        F1: std::future::Future<Output = anyhow::Result<String>>,
        F2: std::future::Future<Output = anyhow::Result<String>>,
    {
        Some(PeerInfo {
            version: version.await.ok()?,
            config_path: config_path.await.unwrap_or_default(),
        })
    }

    let daemon_info = probe(daemon.version(), daemon.config_path()).await;
    let session_info = match session.read().await.as_ref() {
        Some(s) => probe(s.version(), s.config_path()).await,
        None => None,
    };
    let tray_info = match tray.read().await.as_ref() {
        Some(t) => probe(t.version(), t.config_path()).await,
        None => None,
    };
    let cli_version = probe_cli_version().await;
    let rows = make_about_components(
        daemon_info.as_ref(),
        session_info.as_ref(),
        tray_info.as_ref(),
        cli_version.as_deref(),
    );
    on_ui(app_weak, move |app| {
        app.set_about_components(ModelRc::new(VecModel::from(rows)));
    });
}

/// Wrap a fetched [`StateSnapshot`] into Slint models and push to the
/// AppWindow. Must run on the Slint event-loop thread (it builds
/// `ModelRc` instances which are `!Send`).
fn push_snapshot(app: &AppWindow, snapshot: StateSnapshot) {
    let tvs: Vec<TvViewModel> = snapshot
        .tvs
        .into_iter()
        .map(|raw| TvViewModel {
            uuid: raw.uuid,
            name: raw.name,
            vendor: raw.vendor,
            primary_descriptor: raw.primary_descriptor,
            connected: raw.connected,
            power_state: raw.power_state,
            supports_wake: raw.supports_wake,
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
            vendor_config: ModelRc::new(VecModel::from(
                raw.vendor_config
                    .into_iter()
                    .map(|(label, value)| VendorInfoRow {
                        label: SharedString::from(label),
                        value: SharedString::from(value),
                    })
                    .collect::<Vec<_>>(),
            )),
            vendor_info: ModelRc::new(VecModel::from(
                raw.vendor_info
                    .into_iter()
                    .map(|(label, value)| VendorInfoRow {
                        label: SharedString::from(label),
                        value: SharedString::from(value),
                    })
                    .collect::<Vec<_>>(),
            )),
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
