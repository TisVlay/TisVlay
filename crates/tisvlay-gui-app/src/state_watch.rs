//! The GUI's state reconciler. One loop drains each burst of
//! [`StateEvent`]s, then renders the whole view from the clients'
//! mirrors in one event-loop push: TVs, profiles, each service's presence
//! and release, the tray's settings and the version banner. Events only
//! say that something changed, and the render reads what is true now, so
//! a late or coalesced event can't leave a stale value on screen.
//!
//! The render asks no service: an rpc there would hold the loop, and
//! every change after it, behind that service's deadline. What only an
//! rpc tells, and a service fixes for its lifetime (the vendor catalog,
//! the profile floors), is fetched on a second task.

use std::sync::Arc;

use gettextrs::gettext;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tisvlay::{
    DaemonClient, NotificationSettings, Service, SessionClient, StateEvent, StateWatcher,
    TrayClient,
};
use tokio::sync::{Notify, RwLock};
use tracing::debug;

use crate::about::{
    GUI_VERSION, PeerInfo, SupportedVendorsCache, make_about_components, probe_cli_version,
    service_health,
};
use crate::profiles::{self, ProfileEditor};
use crate::state::{ProfileRowRaw, StateSnapshot, fetch_state};
use crate::{
    AppWindow, ErrorState, Errors, PairingWizard, ProfileKind, ProfileRow, ProfileTvRef, Profiles,
    Reconcilers, ServiceHealth, Tray, TrayNotificationSettings, TvViewModel, VendorInfoRow,
    VendorOption, on_ui,
};

/// A service as the render sees it: whether its liveliness token is up,
/// and what its release check learned.
#[derive(Default)]
struct Peer {
    up: bool,
    info: Option<PeerInfo>,
}

impl Peer {
    fn health(&self) -> ServiceHealth {
        service_health(self.up, self.info.as_ref())
    }
}

/// Each live service on a release this GUI can't talk to, with (this
/// GUI's release, the service's), for the version banner. The rule is
/// the one that turns the service's About chip red, so the two always
/// agree.
fn mismatches(peers: &[(Service, &Peer)]) -> Vec<(Service, String, String)> {
    peers
        .iter()
        .filter(|(_, peer)| peer.health() == ServiceHealth::Incompatible)
        .filter_map(|(service, peer)| {
            let info = peer.info.as_ref()?;
            Some((*service, GUI_VERSION.to_owned(), info.version.clone()))
        })
        .collect()
}

/// The version banner for `mismatches`, or none when there are none.
fn version_banner(mismatches: &[(Service, String, String)]) -> ErrorState {
    if mismatches.is_empty() {
        return ErrorState::default();
    }
    let message = mismatches
        .iter()
        .map(|(service, ours, theirs)| {
            let service = service_name(*service);
            gettext("TisVlay {0} can't talk to the {1}, which runs {2}. Restart the {1} or upgrade so the two match.")
                .replace("{0}", ours)
                .replace("{1}", &service)
                .replace("{2}", theirs)
        })
        .collect::<Vec<_>>()
        .join("\n");
    ErrorState {
        kind: SharedString::default(),
        tone: SharedString::from("error"),
        title: SharedString::from(gettext("Version mismatch")),
        message: SharedString::from(message),
        action_label: SharedString::default(),
    }
}

fn service_name(service: Service) -> String {
    match service {
        Service::Daemon => gettext("daemon"),
        Service::Session => gettext("session service"),
        Service::Tray => gettext("tray"),
        _ => service.to_string(),
    }
}

/// Wakes for the lifetime fetches: the watch loop pokes one on each
/// appearance of its service.
#[derive(Default)]
struct Appeared {
    daemon: Notify,
    session: Notify,
}

/// Run the reconciler for the window's lifetime: render once, then once
/// per drained burst of events, beside the lifetime fetches.
pub(crate) async fn watch(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
    supported_vendors_cache: SupportedVendorsCache,
    editor: ProfileEditor,
) {
    // Subscribe before the first read, so a change landing in between
    // still arrives as an event. Session/tray events ride the clients
    // built at startup (resilient links, never replaced), and the watcher
    // synthesizes `*Appeared` for services already up, which starts their
    // lifetime fetches.
    let mut watcher = StateWatcher::new(
        &daemon,
        session.read().await.as_ref(),
        tray.read().await.as_ref(),
        tisvlay::StateWatcherConfig::default(),
    );
    profiles::seed_layers(&daemon, session.read().await.as_ref(), &editor).await;
    let appeared = Appeared::default();
    let mut view = View {
        daemon: daemon.clone(),
        session: session.clone(),
        tray,
        reconcilers,
        app_weak: app_weak.clone(),
        // Read once, since it spawns a process: after a package upgrade
        // the About row shows the new CLI once the GUI restarts.
        cli_version: probe_cli_version().await,
        tray_settings: None,
    };
    view.render().await;
    let events = async {
        while let Some(event) = watcher.next().await {
            let mut next = Some(event);
            while let Some(event) = next {
                apply(event, &daemon, &session, &editor, &app_weak, &appeared).await;
                next = watcher.try_next();
            }
            view.render().await;
        }
    };
    let fetches = fetch_lifetime_facts(
        &daemon,
        &session,
        &app_weak,
        &supported_vendors_cache,
        &editor,
        &appeared,
    );
    // The fetches never end; the events end with the runtime.
    tokio::select! {
        () = events => {}
        () = fetches => {}
    }
}

/// What an event needs beyond the render: an appearance wakes its
/// service's lifetime fetch, and a profile event updates the editor's
/// body cache (mirror reads). Everything else is state the render reads.
async fn apply(
    event: StateEvent,
    daemon: &Arc<DaemonClient>,
    session: &Arc<RwLock<Option<SessionClient>>>,
    editor: &ProfileEditor,
    app_weak: &slint::Weak<AppWindow>,
    appeared: &Appeared,
) {
    match event {
        StateEvent::DaemonAppeared => appeared.daemon.notify_one(),
        StateEvent::SessionAppeared => appeared.session.notify_one(),
        StateEvent::SystemProfileChanged(uuid) => {
            profiles::handle_system_changed(editor.clone(), daemon.clone(), app_weak.clone(), uuid)
                .await;
        }
        StateEvent::SystemProfileAdded(uuid) => {
            profiles::handle_system_added(editor.clone(), daemon.clone(), uuid).await;
        }
        StateEvent::SystemProfileRemoved(uuid) => {
            profiles::handle_system_removed(editor, app_weak.clone(), uuid);
        }
        StateEvent::UserProfileChanged(uuid) => {
            profiles::handle_user_changed(editor.clone(), session.clone(), app_weak.clone(), uuid)
                .await;
        }
        StateEvent::UserProfileAdded(uuid) => {
            profiles::handle_user_added(editor.clone(), session.clone(), uuid).await;
        }
        StateEvent::UserProfileRemoved(uuid) => {
            profiles::handle_user_removed(editor, app_weak.clone(), uuid);
        }
        StateEvent::ReleaseChecked(Service::Daemon) => {
            let sleep = daemon.service_info().await.and_then(|info| info.sleep);
            profiles::set_sleep_support(editor, app_weak.clone(), sleep);
        }
        StateEvent::DaemonStateChanged(_)
        | StateEvent::DaemonDisappeared
        | StateEvent::SessionDisappeared
        | StateEvent::TrayAppeared
        | StateEvent::TrayDisappeared
        | StateEvent::TvUserProfileChanged
        | StateEvent::StreamChanged
        | StateEvent::TrayNotificationSettingsChanged
        | StateEvent::ReleaseChecked(_)
        | StateEvent::VersionMismatch { .. }
        | StateEvent::FallbackPoll => {}
    }
}

/// What the render reads, and the one thing it remembers between renders.
struct View {
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    tray: Arc<RwLock<Option<TrayClient>>>,
    reconcilers: Reconcilers,
    app_weak: slint::Weak<AppWindow>,
    cli_version: Option<String>,
    /// The tray settings last written, so a render writes them only when
    /// they change.
    tray_settings: Option<NotificationSettings>,
}

impl View {
    /// Write the whole view from the clients' mirrors in one event-loop
    /// push. This is the single writer of every property it sets: the TV
    /// and profile lists, each service's presence and health (window
    /// props, the Tray/Profiles globals, the About rows), the version
    /// banner and the tray's switches. Writing them all from one read
    /// keeps the footer, the panes, the About rows and the banner from
    /// ever disagreeing. Mirror reads only; see the module docs.
    async fn render(&mut self) {
        let session = self.session.read().await;
        let tray = self.tray.read().await;
        let mut snapshot = fetch_state(&self.daemon, session.as_ref()).await;
        self.reconcilers.reconcile(&mut snapshot);
        let daemon = Peer {
            up: self.daemon.is_available().await,
            info: self.daemon.service_info().await.map(PeerInfo::from),
        };
        let session_peer = match session.as_ref() {
            Some(client) => Peer {
                up: client.is_available().await,
                info: client.service_info().await.map(PeerInfo::from),
            },
            None => Peer::default(),
        };
        let (tray_peer, tray_settings) = match tray.as_ref() {
            Some(client) => (
                Peer {
                    up: client.is_available().await,
                    info: client.service_info().await.map(PeerInfo::from),
                },
                client.notification_settings().await,
            ),
            None => (Peer::default(), None),
        };
        drop((session, tray));
        // The documented defaults until the tray first publishes; the
        // mirror keeps its last switches while it's down.
        let tray_settings = tray_settings.unwrap_or_default();
        let changed_settings =
            (self.tray_settings != Some(tray_settings)).then(|| settings_to_slint(&tray_settings));
        self.tray_settings = Some(tray_settings);
        let banner = version_banner(&mismatches(&[
            (Service::Daemon, &daemon),
            (Service::Session, &session_peer),
            (Service::Tray, &tray_peer),
        ]));
        let rows = make_about_components(
            &crate::endpoints().site,
            (daemon.up, daemon.info.as_ref()),
            (session_peer.up, session_peer.info.as_ref()),
            (tray_peer.up, tray_peer.info.as_ref()),
            self.cli_version.as_deref(),
        );
        let (daemon_up, session_up, tray_up) = (daemon.up, session_peer.up, tray_peer.up);
        let (daemon_health, session_health, tray_health) =
            (daemon.health(), session_peer.health(), tray_peer.health());
        on_ui(&self.app_weak, move |app| {
            push_snapshot(&app, snapshot);
            app.set_daemon_available(daemon_up);
            app.set_session_available(session_up);
            app.set_tray_available(tray_up);
            app.global::<Tray>().set_available(tray_up);
            app.global::<Profiles>().set_session_available(session_up);
            app.set_daemon_health(daemon_health);
            app.set_session_health(session_health);
            app.set_tray_health(tray_health);
            app.set_about_components(ModelRc::new(VecModel::from(rows)));
            app.global::<Errors>().set_version(banner);
            if let Some(settings) = changed_settings {
                app.global::<Tray>().set_notifications(settings);
            }
        });
    }
}

/// Fetch what a service fixes for its lifetime and only an rpc tells:
/// the daemon's vendor catalog and system-profile floor, the session's
/// user-profile floor. Runs beside the watch loop, so a service that
/// stops mid-fetch holds up only this. A wake during a fetch queues one
/// more (a [`Notify`] keeps one permit), so the last fetch always follows
/// the last appearance.
async fn fetch_lifetime_facts(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    app_weak: &slint::Weak<AppWindow>,
    supported_vendors_cache: &SupportedVendorsCache,
    editor: &ProfileEditor,
    appeared: &Appeared,
) {
    loop {
        tokio::select! {
            () = appeared.daemon.notified() => {
                // Gone again already: its next appearance wakes this.
                if daemon.is_available().await {
                    fetch_and_publish_vendors(daemon, app_weak, supported_vendors_cache).await;
                    profiles::fetch_system_floor(daemon, editor, app_weak).await;
                }
            }
            () = appeared.session.notified() => {
                let client = session.read().await.as_ref().cloned();
                if let Some(client) = client
                    && client.is_available().await
                {
                    profiles::fetch_user_floor(&client, editor, app_weak).await;
                }
            }
        }
    }
}

/// Fetch the daemon's supported-vendor list (the `manager/vendors`
/// catalog) and publish it to the pairing-wizard global plus the
/// diagnostic-blob cache. Sorted by display name (case-insensitive) so
/// the wizard's ComboBox renders in stable, scannable order regardless of
/// registration order. Runs once per daemon lifetime, from
/// [`fetch_lifetime_facts`] alone, which makes it the only writer of
/// those properties.
async fn fetch_and_publish_vendors(
    daemon: &DaemonClient,
    app_weak: &slint::Weak<AppWindow>,
    supported_vendors_cache: &SupportedVendorsCache,
) {
    let mut vendors = match daemon.get_supported_vendors().await {
        Ok(vendors) => vendors,
        Err(e) => {
            debug!(error = %e, "vendor catalog unavailable");
            Vec::new()
        }
    };
    vendors.sort_by_key(|vendor| vendor.name.to_lowercase());
    debug!(
        count = vendors.len(),
        "Fetched supported vendors from daemon"
    );
    if let Ok(mut cache) = supported_vendors_cache.write() {
        *cache = vendors.iter().map(|vendor| vendor.id.clone()).collect();
    }

    // The catalog carries each vendor's capabilities; the pairing
    // schema is one rpc per vendor. Vendor counts are O(1) in practice
    // (one to a handful), so the round-trips are negligible against any
    // user-visible wizard step.
    let mut vendors_with_caps: Vec<(String, String, tisvlay_core::Capabilities, PrimaryField)> =
        Vec::with_capacity(vendors.len());
    for vendor in vendors {
        let id = vendor.id;
        let name = vendor.name;
        let names: Vec<&str> = vendor.capabilities.iter().map(String::as_str).collect();
        let caps = tisvlay_core::Capabilities::from_strings(&names);
        let schema = daemon.get_pairing_schema(&id).await;
        // The schema's first field is the vendor's primary descriptor
        // (schema-ordering convention) — it becomes the manual-entry
        // panel's identity input. Fetch failure falls back to a
        // generic "Address" so manual entry stays usable.
        let primary = schema
            .ok()
            .and_then(|schema| schema.into_iter().next())
            .map(|field| {
                let is_ip = field.field_type == tisvlay_core::PairingFieldType::Ip;
                let placeholder = if !field.placeholder.is_empty() {
                    field.placeholder
                } else if is_ip
                    && caps.contains(tisvlay_core::Capabilities::IPV6)
                    && !caps.contains(tisvlay_core::Capabilities::IPV4)
                {
                    // v6-only vendors get the RFC 3849 documentation
                    // prefix; everything else keeps the familiar v4.
                    "2001:db8::1".into()
                } else if is_ip {
                    "192.168.1.100".into()
                } else {
                    String::new()
                };
                PrimaryField {
                    label: field.label,
                    placeholder,
                    is_ip,
                }
            })
            .unwrap_or_default();
        vendors_with_caps.push((id, name, caps, primary));
    }

    on_ui(app_weak, move |app| {
        let wizard = app.global::<PairingWizard>();
        let names: Vec<SharedString> = vendors_with_caps
            .iter()
            .map(|(_, name, _, _)| SharedString::from(name.as_str()))
            .collect();
        let vendors: Vec<VendorOption> = vendors_with_caps
            .into_iter()
            .map(|(id, name, caps, primary)| VendorOption {
                id: SharedString::from(id),
                name: SharedString::from(name),
                supports_ipv4: caps.contains(tisvlay_core::Capabilities::IPV4),
                supports_ipv6: caps.contains(tisvlay_core::Capabilities::IPV6),
                primary_label: SharedString::from(if primary.label.is_empty() {
                    gettext("Address")
                } else {
                    primary.label
                }),
                primary_placeholder: SharedString::from(primary.placeholder),
                primary_is_ip: primary.is_ip,
            })
            .collect();
        wizard.set_supported_vendors(ModelRc::new(VecModel::from(vendors)));
        wizard.set_supported_vendor_names(ModelRc::new(VecModel::from(names)));
    });
}

/// Manual-entry identity-input shape derived from a vendor schema's
/// first field.
#[derive(Default)]
struct PrimaryField {
    label: String,
    placeholder: String,
    is_ip: bool,
}

/// Lower the tray's settings into the Slint `TrayNotificationSettings`
/// struct. (`TrayNotificationSettings` is Slint-generated, so the field
/// list can't route through `NotificationCategory::ALL`.)
fn settings_to_slint(settings: &NotificationSettings) -> TrayNotificationSettings {
    TrayNotificationSettings {
        tv_connection: settings.tv_connection,
        tv_pairing: settings.tv_pairing,
        tv_wake: settings.tv_wake,
        daemon_offline: settings.daemon_offline,
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
            uuid: raw.uuid,
            name: raw.name,
            vendor: raw.vendor,
            brand: raw.brand,
            primary_descriptor: raw.primary_descriptor,
            connected: raw.connected,
            waiting_for: raw.waiting_for,
            power_state: raw.power_state,
            is_on: raw.is_on,
            supports_wake: raw.supports_wake,
            has_screen: raw.has_screen,
            has_volume: raw.has_volume,
            has_mute: raw.has_mute,
            has_input: raw.has_input,
            has_backlight: raw.has_backlight,
            has_remote_power: raw.has_remote_power,
            has_remote_keys: raw.has_remote_keys,
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
        app.set_system_profiles(m);
    });
    sync_vec_model(&app.get_user_profiles(), snapshot.user_profiles, |m| {
        app.set_user_profiles(m);
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
        profiles.set_system(m);
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

#[cfg(test)]
mod version_banner_tests {
    use super::*;

    fn raw_tv(uuid: &str, name: &str, connected: bool) -> crate::state::TvViewRaw {
        crate::state::TvViewRaw {
            uuid: SharedString::from(uuid),
            name: SharedString::from(name),
            vendor: SharedString::from("mock"),
            brand: SharedString::from("Mock"),
            primary_descriptor: SharedString::from("mock://tv"),
            connected,
            waiting_for: SharedString::default(),
            power_state: SharedString::from(if connected { "active" } else { "unknown" }),
            is_on: connected,
            supports_wake: true,
            has_screen: true,
            has_volume: true,
            has_mute: true,
            has_input: true,
            has_backlight: true,
            has_remote_power: true,
            has_remote_keys: true,
            is_screen_on: connected,
            volume: 42,
            muted: false,
            backlight: 60,
            current_input: SharedString::from("HDMI_1"),
            available_input_ids: vec![SharedString::from("HDMI_1"), SharedString::from("HDMI_2")],
            system_profile: SharedString::from("Work"),
            system_profile_uuid: SharedString::from("system-profile"),
            user_profile: SharedString::from("Personal"),
            user_profile_uuid: SharedString::from("user-profile"),
            primary_input: SharedString::from("HDMI_1"),
            managed: true,
            automated: connected,
            hw_manufacturer: SharedString::from("Maker"),
            hw_model: SharedString::from("Model"),
            hw_serial: SharedString::from("Serial"),
            desktop_connector: SharedString::from("card1-HDMI-A-1"),
            vendor_config: vec![("MAC".into(), "aa:bb:cc:dd:ee:ff".into())],
            vendor_info: vec![("transport".into(), "wss".into())],
        }
    }

    #[test]
    fn no_mismatch_no_banner() {
        assert_eq!(version_banner(&[]).title, "");
    }

    #[test]
    fn each_mismatched_service_gets_a_line() {
        let banner = version_banner(&[
            (Service::Daemon, "0.4.0".into(), "0.3.2".into()),
            (Service::Tray, "0.4.0".into(), "0.5.0".into()),
        ]);
        assert_eq!(banner.title, "Version mismatch");
        assert_eq!(
            banner.message,
            "TisVlay 0.4.0 can't talk to the daemon, which runs 0.3.2. Restart the daemon or upgrade so the two match.\nTisVlay 0.4.0 can't talk to the tray, which runs 0.5.0. Restart the tray or upgrade so the two match."
        );
    }

    #[test]
    fn only_a_live_service_on_another_release_line_is_a_mismatch() {
        let peer = |up, version: Option<&str>| Peer {
            up,
            info: version.map(|version| PeerInfo {
                version: version.into(),
                config_path: String::new(),
            }),
        };
        let same = peer(true, Some(GUI_VERSION));
        let unread = peer(true, None);
        let stopped = peer(false, Some("99.0.0"));
        let other = peer(true, Some("99.0.0"));
        assert!(
            mismatches(&[
                (Service::Daemon, &same),
                (Service::Session, &unread),
                (Service::Tray, &stopped),
            ])
            .is_empty()
        );
        assert_eq!(
            mismatches(&[(Service::Daemon, &same), (Service::Tray, &other)]),
            [(Service::Tray, GUI_VERSION.to_owned(), "99.0.0".to_owned())]
        );
    }

    #[test]
    fn an_empty_snapshot_clears_stale_rows_and_the_connected_count() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let stale = TvViewModel {
            name: SharedString::from("Stale TV"),
            connected: true,
            ..Default::default()
        };
        app.set_tvs(ModelRc::new(VecModel::from(vec![stale])));
        app.set_tvs_connected_count(1);

        push_snapshot(
            &app,
            StateSnapshot {
                tvs: Vec::new(),
                system_profiles: Vec::new(),
                user_profiles: Vec::new(),
                system_profile_rows: Vec::new(),
                user_profile_rows: Vec::new(),
            },
        );

        assert_eq!(app.get_tvs().row_count(), 0);
        assert_eq!(app.get_tvs_connected_count(), 0);
    }

    #[test]
    fn tray_notification_settings_project_each_category_without_swapping() {
        let projected = settings_to_slint(&NotificationSettings {
            tv_connection: true,
            tv_pairing: false,
            tv_wake: true,
            daemon_offline: false,
        });

        assert!(projected.tv_connection);
        assert!(!projected.tv_pairing);
        assert!(projected.tv_wake);
        assert!(!projected.daemon_offline);
    }

    #[test]
    fn push_snapshot_refreshes_selected_profile_and_clears_deleted_selection() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");
        let profiles = app.global::<Profiles>();
        profiles.set_selected_kind(ProfileKind::System);
        profiles.set_selected_uuid(SharedString::from("system-profile"));

        push_snapshot(
            &app,
            StateSnapshot {
                tvs: Vec::new(),
                system_profiles: vec![SharedString::from("Default")],
                user_profiles: vec![SharedString::from("Personal")],
                system_profile_rows: vec![ProfileRowRaw {
                    uuid: SharedString::from("system-profile"),
                    name: SharedString::from("Work"),
                    display_name: SharedString::from("Work *"),
                    is_default: false,
                    referenced_by: vec![crate::state::ProfileTvRefRaw {
                        uuid: SharedString::from("tv-1"),
                        name: SharedString::from("Office"),
                        automated: true,
                    }],
                    referenced_by_summary: SharedString::from("Office"),
                    active: true,
                }],
                user_profile_rows: vec![ProfileRowRaw {
                    uuid: SharedString::from("user-profile"),
                    name: SharedString::from("Personal"),
                    display_name: SharedString::from("Personal"),
                    is_default: true,
                    referenced_by: Vec::new(),
                    referenced_by_summary: SharedString::default(),
                    active: false,
                }],
            },
        );

        let system_rows = profiles.get_system();
        let system_row = system_rows
            .row_data(0)
            .expect("the system profile row is projected");
        assert_eq!(system_row.display_name, "Work *");
        assert_eq!(system_row.referenced_by_summary, "Office");
        assert!(system_row.active);
        let references = system_row.referenced_by;
        let reference = references
            .row_data(0)
            .expect("the profile reference is projected");
        assert_eq!(reference.name, "Office");
        assert!(reference.automated);
        assert_eq!(profiles.get_selected_row().display_name, "Work *");
        assert_eq!(
            app.get_system_profiles().row_data(0).as_deref(),
            Some("Default")
        );
        assert_eq!(profiles.get_user().row_count(), 1);
        assert_eq!(
            profiles.get_user().row_data(0).unwrap().display_name,
            "Personal"
        );

        profiles.set_selected_kind(ProfileKind::User);
        profiles.set_selected_uuid(SharedString::from("user-profile"));
        push_snapshot(
            &app,
            StateSnapshot {
                tvs: Vec::new(),
                system_profiles: Vec::new(),
                user_profiles: Vec::new(),
                system_profile_rows: Vec::new(),
                user_profile_rows: vec![ProfileRowRaw {
                    uuid: SharedString::from("user-profile"),
                    name: SharedString::from("Personal"),
                    display_name: SharedString::from("Personal (edited)"),
                    is_default: true,
                    referenced_by: Vec::new(),
                    referenced_by_summary: SharedString::default(),
                    active: true,
                }],
            },
        );
        assert_eq!(
            profiles.get_selected_row().display_name,
            "Personal (edited)"
        );

        push_snapshot(
            &app,
            StateSnapshot {
                tvs: Vec::new(),
                system_profiles: Vec::new(),
                user_profiles: Vec::new(),
                system_profile_rows: Vec::new(),
                user_profile_rows: Vec::new(),
            },
        );
        assert!(profiles.get_selected_row().uuid.is_empty());
        assert_eq!(profiles.get_system().row_count(), 0);
    }

    #[test]
    fn push_snapshot_projects_tv_rows_and_connected_count() {
        i_slint_backend_testing::init_no_event_loop();
        let app = AppWindow::new().expect("AppWindow::new");

        push_snapshot(
            &app,
            StateSnapshot {
                tvs: vec![
                    raw_tv("tv-1", "Office", true),
                    raw_tv("tv-2", "Bedroom", false),
                ],
                system_profiles: Vec::new(),
                user_profiles: Vec::new(),
                system_profile_rows: Vec::new(),
                user_profile_rows: Vec::new(),
            },
        );

        let tvs = app.get_tvs();
        assert_eq!(tvs.row_count(), 2);
        assert_eq!(app.get_tvs_connected_count(), 1);
        let office = tvs.row_data(0).expect("the first TV row is projected");
        assert_eq!(office.uuid, "tv-1");
        assert_eq!(office.name, "Office");
        assert_eq!(office.brand, "Mock");
        assert_eq!(office.current_input, "HDMI_1");
        assert!(office.has_volume);
        assert!(office.automated);
        assert!(office.power_cycle_phase.is_empty());
        let inputs = office.available_input_ids;
        assert_eq!(inputs.row_count(), 2);
        assert_eq!(inputs.row_data(1).as_deref(), Some("HDMI_2"));
        let vendor_config = office.vendor_config;
        assert_eq!(vendor_config.row_data(0).unwrap().label, "MAC");
        let vendor_info = office.vendor_info;
        assert_eq!(vendor_info.row_data(0).unwrap().value, "wss");

        let bedroom = tvs.row_data(1).expect("the second TV row is projected");
        assert!(!bedroom.connected);
        assert!(!bedroom.is_on);
        assert!(!bedroom.automated);
    }

    #[test]
    fn sync_vec_model_updates_existing_rows_and_tracks_length_changes() {
        let current = ModelRc::new(VecModel::from(vec![1, 2]));

        sync_vec_model(&current, vec![3, 4, 5], |_| {
            panic!("a VecModel should be updated in place")
        });

        let model = current
            .as_any()
            .downcast_ref::<VecModel<i32>>()
            .expect("the model remains a VecModel");
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.row_data(0), Some(3));
        assert_eq!(model.row_data(2), Some(5));

        sync_vec_model(&current, vec![9], |_| {
            panic!("shrinking a VecModel should not replace it")
        });
        assert_eq!(model.row_count(), 1);
        assert_eq!(model.row_data(0), Some(9));
    }
}
