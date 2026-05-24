//! Long-running state-watch loop. Drives the bus-signal subscription
//! that feeds GUI refreshes, applies value-ramping reconcilers, and
//! pushes view-model snapshots into the AppWindow's models.

use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay_core::{
    DaemonClient, NotificationSettingsBody, RefreshTrigger, SessionClient, StateEvent,
    StateWatcher, TrayClient,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::about::{PeerInfo, SupportedVendorsCache, make_about_components, probe_cli_version};
use crate::profiles::{self, ProfileEditor};
use crate::state::{ProfileRowRaw, StateSnapshot, fetch_state};
use crate::{
    AppWindow, PairingWizard, ProfileKind, ProfileRow, ProfileTvRef, Profiles, Reconcilers, Tray,
    TrayNotificationSettings, TvViewModel, VendorInfoRow, VendorOption, on_ui,
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
    editor: ProfileEditor,
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

    // Prime baseline + default body for each kind. The bodies catalog
    // stays lazy — only the current selection's UUID gets fetched on
    // demand by `profiles::select`. Errors here are non-fatal; the
    // baselines fall back to all-`None` and the inherit walk lands
    // on an empty resolved value rather than blowing up the editor.
    profiles::prime_system_layers(&daemon, &editor).await;
    if let Some(client) = session.read().await.as_ref() {
        profiles::prime_user_layers(client, &editor).await;
    }

    // Fetch + publish the daemon's vendor list. If the daemon is
    // offline at startup the call returns an empty list and the
    // wizard's ComboBox renders blank; `watch_state` re-runs this on
    // `DaemonAppeared` so a GUI launched before the daemon recovers
    // once the daemon comes up.
    fetch_and_publish_vendors(&daemon, &app_weak, &supported_vendors_cache).await;

    let initial_daemon_available = initial_snapshot.is_some();
    // Seed `Tray.notifications` with documented defaults from core
    // before any successful fetch. Gives the settings pane sensible
    // switch positions even if the GUI starts while the tray is
    // offline; gets overwritten with real values once
    // `refresh_tray_notifications` lands a fetch.
    let initial_tray_notifications = body_to_slint(&NotificationSettingsBody::defaults());
    on_ui(&app_weak, move |app| {
        if let Some(snapshot) = initial_snapshot {
            push_snapshot(&app, snapshot);
        }
        app.set_daemon_available(initial_daemon_available);
        app.set_session_available(initial_session_available);
        app.set_tray_available(initial_tray_available);
        app.global::<Tray>()
            .set_notifications(initial_tray_notifications);
        // Profiles-pane User section needs its own copy of the
        // session-bus availability flag so the "Session service
        // unavailable" hint and the disabled "+" button can bind
        // against the Profiles global directly.
        app.global::<Profiles>()
            .set_session_available(initial_session_available);
    });

    if initial_tray_available {
        refresh_tray_notifications(&tray, &app_weak).await;
    }

    refresh_about_components(&daemon, &session, &tray, &app_weak).await;
    watch_state(
        daemon,
        session,
        tray,
        reconcilers,
        app_weak,
        supported_vendors_cache,
        editor,
    )
    .await;
}

/// Fetch the daemon's supported-vendor list (`Manager1.GetSupportedVendors`)
/// and publish it to the pairing-wizard global plus the diagnostic-blob
/// cache. Sorted by display name (case-insensitive) so the wizard's
/// ComboBox renders in stable, scannable order regardless of registration
/// order.
///
/// Idempotent — safe to call multiple times. Vendor lists are static
/// per daemon binary today, so the second call is wasted work in the
/// common case. Re-running on `DaemonAppeared` covers the regression
/// case where the GUI started before the daemon: the initial fetch
/// returns empty (`unwrap_or_default()`), without a re-fetch the
/// wizard's vendor ComboBox stays blank until the GUI restarts.
async fn fetch_and_publish_vendors(
    daemon: &DaemonClient,
    app_weak: &slint::Weak<AppWindow>,
    supported_vendors_cache: &SupportedVendorsCache,
) {
    let mut vendor_pairs = daemon.get_supported_vendors().await.unwrap_or_default();
    vendor_pairs.sort_by_key(|a| a.1.to_lowercase());
    debug!(
        count = vendor_pairs.len(),
        "Fetched supported vendors from daemon"
    );
    if let Ok(mut cache) = supported_vendors_cache.write() {
        *cache = vendor_pairs.iter().map(|(id, _)| id.clone()).collect();
    }

    // Per-vendor capability fetch in a second pass — keeps the wire
    // payload of `GetSupportedVendors` unchanged (still `a(ss)`) and
    // lets vendors come and go without renegotiating shape. Vendor
    // counts are O(1) in practice (one to a handful), so the extra
    // round-trips are negligible against the cost of any user-visible
    // wizard step. Errors per vendor collapse to "no IP family
    // support claimed", which means the wizard's affordances stay
    // conservative — placeholder reverts to v4-only — rather than
    // silently asserting transport support the daemon didn't
    // actually confirm.
    let mut vendors_with_caps: Vec<(String, String, tisvlay_core::Capabilities)> =
        Vec::with_capacity(vendor_pairs.len());
    for (id, name) in vendor_pairs {
        let caps = daemon
            .get_vendor_capabilities(&id)
            .await
            .unwrap_or_else(|e| {
                debug!(vendor = %id, error = %e, "GetVendorCapabilities failed; treating as empty");
                tisvlay_core::Capabilities::empty()
            });
        vendors_with_caps.push((id, name, caps));
    }

    on_ui(app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        let names: Vec<SharedString> = vendors_with_caps
            .iter()
            .map(|(_, name, _)| SharedString::from(name.as_str()))
            .collect();
        let vendors: Vec<VendorOption> = vendors_with_caps
            .into_iter()
            .map(|(id, name, caps)| VendorOption {
                id: SharedString::from(id),
                name: SharedString::from(name),
                supports_ipv4: caps.contains(tisvlay_core::Capabilities::IPV4),
                supports_ipv6: caps.contains(tisvlay_core::Capabilities::IPV6),
            })
            .collect();
        wizard.set_supported_vendors(ModelRc::new(VecModel::from(vendors)));
        wizard.set_supported_vendor_names(ModelRc::new(VecModel::from(names)));
    });
}

async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
    supported_vendors_cache: SupportedVendorsCache,
    editor: ProfileEditor,
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
            StateEvent::DaemonAppeared => {
                // Republish the vendor list — covers the GUI-started-
                // before-the-daemon case where the initial fetch
                // returned empty. Vendor lists are static per daemon
                // binary, so a duplicate fetch is wasted work but
                // cheap; `DaemonAppeared` isn't high-frequency.
                fetch_and_publish_vendors(&daemon, &app_weak, &supported_vendors_cache).await;
                // Re-prime the system-side baseline + default body
                // — the daemon may have been restarted with edited
                // defaults on disk, in which case the inherit walk
                // would otherwise resolve against a stale ancestor.
                profiles::prime_system_layers(&daemon, &editor).await;
                trigger_refresh();
            }
            StateEvent::DaemonStateChanged(_)
            | StateEvent::TvUserProfileChanged
            | StateEvent::FallbackPoll => {
                trigger_refresh();
            }
            StateEvent::SystemProfileChanged(uuid) => {
                // Editor cache + selected-row refresh. The master
                // list also needs a redraw because `referenced_by`
                // chip counts can shift when a profile's `name`
                // changes.
                let daemon = daemon.clone();
                let editor = editor.clone();
                let app_weak = app_weak.clone();
                tokio::spawn(async move {
                    profiles::handle_system_changed(editor, daemon, app_weak, uuid).await;
                });
                trigger_refresh();
            }
            StateEvent::SystemProfileAdded(uuid) => {
                let daemon = daemon.clone();
                let editor = editor.clone();
                tokio::spawn(async move {
                    profiles::handle_system_added(editor, daemon, uuid).await;
                });
                trigger_refresh();
            }
            StateEvent::SystemProfileRemoved(uuid) => {
                profiles::handle_system_removed(&editor, app_weak.clone(), uuid);
                trigger_refresh();
            }
            StateEvent::UserProfileChanged(uuid) => {
                let session = session.clone();
                let editor = editor.clone();
                let app_weak = app_weak.clone();
                tokio::spawn(async move {
                    profiles::handle_user_changed(editor, session, app_weak, uuid).await;
                });
                trigger_refresh();
            }
            StateEvent::UserProfileAdded(uuid) => {
                let session = session.clone();
                let editor = editor.clone();
                tokio::spawn(async move {
                    profiles::handle_user_added(editor, session, uuid).await;
                });
                trigger_refresh();
            }
            StateEvent::UserProfileRemoved(uuid) => {
                profiles::handle_user_removed(&editor, app_weak.clone(), uuid);
                trigger_refresh();
            }
            StateEvent::DaemonDisappeared => {
                on_ui(&app_weak, move |app| {
                    app.set_daemon_available(false);
                    app.set_tvs(ModelRc::new(VecModel::<TvViewModel>::default()));
                    app.set_tvs_connected_count(0);
                });
                refresh_about_components(&daemon, &session, &tray, &app_weak).await;
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await
                    && client.is_available().await
                {
                    profiles::prime_user_layers(&client, &editor).await;
                    *session.write().await = Some(client);
                    on_ui(&app_weak, move |app| {
                        app.set_session_available(true);
                        app.global::<Profiles>().set_session_available(true);
                    });
                    trigger_refresh();
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                on_ui(&app_weak, move |app| {
                    app.set_session_available(false);
                    app.global::<Profiles>().set_session_available(false);
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
                    refresh_tray_notifications(&tray, &app_weak).await;
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
            StateEvent::TrayNotificationSettingsChanged => {
                // Property-change signal from the tray's
                // `Tray1.NotificationSettings` — re-fetch the body
                // and update the Slint global. Switch values get
                // their new bindings on the next render.
                refresh_tray_notifications(&tray, &app_weak).await;
            }
            StateEvent::SessionAutomationFired { .. } => {
                // Tray surfaces this as a desktop notification; the
                // GUI doesn't render automation history, so nothing
                // to do.
            }
        }
    }
}

/// Pull the current notification body from the tray and push into the
/// `Tray.notifications` Slint global. Used on `TrayAppeared` (initial
/// sync after the tray comes up) and on `TrayNotificationSettingsChanged`
/// (every subsequent mutation, whether via D-Bus or a hand-edit +
/// `ReloadConfig`). Tray-offline → no-op (the global keeps its
/// last-fetched values for the visible-but-disabled UX).
async fn refresh_tray_notifications(
    tray: &RwLock<Option<TrayClient>>,
    app_weak: &slint::Weak<AppWindow>,
) {
    let body = {
        let guard = tray.read().await;
        match guard.as_ref() {
            Some(client) => match client.notification_settings().await {
                Ok(body) => body,
                Err(e) => {
                    debug!(error = %e, "Tray notification_settings fetch failed");
                    return;
                }
            },
            None => return,
        }
    };
    let settings = body_to_slint(&body);
    on_ui(app_weak, move |app| {
        app.global::<Tray>().set_notifications(settings);
    });
}

/// Lower a `NotificationSettingsBody` (every field `Option<bool>`)
/// into the Slint `TrayNotificationSettings` struct (every field
/// `bool`). The tray's property getter populates every field
/// (`to_body` fills them); defaults from
/// `NotificationSettingsBody::defaults()` are the safe fallback for
/// the theoretical case where a future tray returns a partial body
/// (forward-compat with version skew).
fn body_to_slint(body: &NotificationSettingsBody) -> TrayNotificationSettings {
    let defaults = NotificationSettingsBody::defaults();
    TrayNotificationSettings {
        tv_connection: body
            .tv_connection
            .or(defaults.tv_connection)
            .unwrap_or(true),
        tv_pairing: body.tv_pairing.or(defaults.tv_pairing).unwrap_or(true),
        tv_wake: body.tv_wake.or(defaults.tv_wake).unwrap_or(true),
        automation_fired: body
            .automation_fired
            .or(defaults.automation_fired)
            .unwrap_or(false),
        config_reloaded: body
            .config_reloaded
            .or(defaults.config_reloaded)
            .unwrap_or(false),
        daemon_offline: body
            .daemon_offline
            .or(defaults.daemon_offline)
            .unwrap_or(true),
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
            system_profile_uuid: raw.system_profile_uuid,
            user_profile: raw.user_profile,
            user_profile_uuid: raw.user_profile_uuid,
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
    // Aggregate `tv.connected` here — Slint can't fold over a model
    // in a pure-binding context, so the GUI's master-list header
    // reads this precomputed value.
    let connected_count = tvs.iter().filter(|t| t.connected).count() as i32;
    sync_vec_model(&app.get_tvs(), tvs, |m| app.set_tvs(m));
    app.set_tvs_connected_count(connected_count);
    sync_vec_model(&app.get_system_profiles(), snapshot.system_profiles, |m| {
        app.set_system_profiles(m)
    });
    sync_vec_model(&app.get_user_profiles(), snapshot.user_profiles, |m| {
        app.set_user_profiles(m)
    });

    // Profiles pane master lists. ProfileRow is generated from
    // `ui/globals/profiles.slint` and exported through `main.slint`;
    // the `referenced_by` inner list is wrapped here on the event-
    // loop thread because Slint's `ModelRc` is `!Send`.
    let profiles = app.global::<Profiles>();
    let system_rows: Vec<ProfileRow> = snapshot
        .system_profile_rows
        .into_iter()
        .map(profile_row_into_slint)
        .collect();
    let user_rows: Vec<ProfileRow> = snapshot
        .user_profile_rows
        .into_iter()
        .map(profile_row_into_slint)
        .collect();

    // Re-resolve `Profiles.selected-row` from the freshly-built
    // master-list rows. The click handlers in `profiles_pane.slint`
    // write `selected-row` as a value-copy at click time (for
    // same-frame detail-pane materialization); without this re-
    // resolve any field the detail header binds — Used-by chips +
    // their per-TV automated tint, `active` aggregate,
    // `referenced_by_summary`, `display-name` after rename — would
    // stale until the user picked the row again. By looking up
    // matching `selected-uuid` in the new rows here, every refresh
    // tick keeps the snapshot in sync. If the selection went away
    // (profile deleted by another client), the lookup misses and
    // we push a blank row so the detail-pane gate flips back to
    // the empty state — mirrors the per-removal handler in
    // `profiles::clear_selection_in_slint`.
    let selected_uuid = profiles.get_selected_uuid();
    if !selected_uuid.is_empty() {
        let lookup = match profiles.get_selected_kind() {
            ProfileKind::System => system_rows
                .iter()
                .find(|r| r.uuid == selected_uuid)
                .cloned(),
            ProfileKind::User => user_rows.iter().find(|r| r.uuid == selected_uuid).cloned(),
            ProfileKind::None => None,
        };
        profiles.set_selected_row(lookup.unwrap_or_else(blank_profile_row));
    }

    sync_vec_model(&profiles.get_system(), system_rows, |m| {
        profiles.set_system(m)
    });
    sync_vec_model(&profiles.get_user(), user_rows, |m| profiles.set_user(m));
}

/// Empty `ProfileRow` value used when the previously-selected
/// profile has been removed from the catalog. The detail-pane
/// gate in `ProfilesPane` reads `selected-row.uuid != ""`, so a
/// blank-uuid push flips the gate back to "Select a profile".
/// Mirrored from the inline literal in `profiles::clear_selection_in_slint`.
fn blank_profile_row() -> ProfileRow {
    ProfileRow {
        uuid: SharedString::default(),
        name: SharedString::default(),
        display_name: SharedString::default(),
        is_default: false,
        referenced_by: ModelRc::new(VecModel::<ProfileTvRef>::default()),
        referenced_by_summary: SharedString::default(),
        active: false,
    }
}

/// Wrap one Rust-side profile row into the Slint-generated `ProfileRow`
/// shape — `referenced_by` becomes a `ModelRc` here because Slint's
/// model handles are `!Send` and can only be built on the event-loop
/// thread.
fn profile_row_into_slint(raw: ProfileRowRaw) -> ProfileRow {
    let refs: Vec<ProfileTvRef> = raw
        .referenced_by
        .into_iter()
        .map(|r| ProfileTvRef {
            uuid: r.uuid,
            name: r.name,
            automated: r.automated,
        })
        .collect();
    ProfileRow {
        uuid: raw.uuid,
        name: raw.name,
        display_name: raw.display_name,
        is_default: raw.is_default,
        referenced_by: ModelRc::new(VecModel::from(refs)),
        referenced_by_summary: raw.referenced_by_summary,
        active: raw.active,
    }
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
