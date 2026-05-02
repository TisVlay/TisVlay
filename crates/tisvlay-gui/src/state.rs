//! Fetches TV state from the daemon and maps it into the Slint
//! `TvViewModel` struct generated from `ui/main.slint`.
//!
//! The fetched data lives in plain `Vec`-based structs (`TvViewRaw`)
//! so it crosses the tokio→Slint thread boundary as `Send`. The Slint
//! event-loop closure then wraps the inner per-row lists into
//! `ModelRc` (which holds `Rc`, `!Send`).

use anyhow::Result;
use slint::SharedString;
use tisvlay_core::{DaemonClient, SessionClient};
use tracing::debug;

/// Per-TV view-model row in a thread-safe shape (no `Rc` inside).
/// Converted to the Slint-generated `TvViewModel` inside the event
/// loop, where `ModelRc::new` is allowed.
pub(crate) struct TvViewRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub vendor: SharedString,
    pub ip: SharedString,
    pub connected: bool,
    pub power_state: SharedString,
    pub supports_wake: bool,
    pub is_screen_on: bool,
    pub volume: i32,
    pub muted: bool,
    pub backlight: i32,
    pub current_input: SharedString,
    pub available_input_ids: Vec<SharedString>,
    pub system_profile: SharedString,
    pub user_profile: SharedString,
    pub primary_input: SharedString,
    pub managed: bool,
    pub automated: bool,
    pub hw_manufacturer: SharedString,
    pub hw_model: SharedString,
    pub hw_serial: SharedString,
    pub desktop_connector: SharedString,
    /// Vendor-specific diagnostic rows from `Tv1.VendorInfo`.
    /// Plain `Vec<(String, String)>` here so the snapshot crosses
    /// the tokio→Slint thread boundary as `Send`; the conversion
    /// to `[VendorInfoRow]` (a `ModelRc`-backed list) happens
    /// inside the event-loop closure in `main.rs`.
    pub vendor_info: Vec<(String, String)>,
}

/// Snapshot of fetched state for one refresh pass.
pub(crate) struct StateSnapshot {
    pub tvs: Vec<TvViewRaw>,
    pub system_profiles: Vec<SharedString>,
    pub user_profiles: Vec<SharedString>,
}

/// Fetch every configured TV's state plus session-side user-profile
/// assignment + global profile lists for ComboBox population.
///
/// Single round-trip on the system bus (`GetManagedObjects` via
/// `DaemonClient::list_tvs`), one daemon call for system profiles,
/// one optional session-bus call for user profiles, and one optional
/// session-bus call per TV for the user-profile assignment.
pub(crate) async fn fetch_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> Result<StateSnapshot> {
    let tvs = daemon.list_tvs().await?;
    debug!(count = tvs.len(), "Fetched TVs from daemon");

    let system_profiles: Vec<SharedString> = daemon
        .list_system_profiles()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(name, _)| SharedString::from(name))
        .collect();

    let user_profiles: Vec<SharedString> = match session {
        Some(s) => s
            .list_user_profiles()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(name, _)| SharedString::from(name))
            .collect(),
        None => Vec::new(),
    };

    let mut models = Vec::with_capacity(tvs.len());
    for tv in tvs {
        let user_profile = match session {
            Some(s) => s
                .get_tv_user_profile(&tv.uuid)
                .await
                .ok()
                .unwrap_or_default(),
            None => String::new(),
        };

        let available_input_ids: Vec<SharedString> = tv
            .inputs
            .iter()
            .map(|(id, _)| SharedString::from(id.as_str()))
            .collect();

        // Spatial hint shown alongside the name — backend's primary
        // diagnostic descriptor (LG: IP; future Bluetooth: MAC; etc.).
        // First non-Secret entry of `Tv1.VendorInfo` by convention.
        let primary_descriptor = tv
            .vendor_info
            .first()
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        models.push(TvViewRaw {
            uuid: SharedString::from(tv.uuid),
            name: SharedString::from(tv.name),
            vendor: SharedString::from(tv.vendor),
            ip: SharedString::from(primary_descriptor),
            connected: tv.connected,
            power_state: SharedString::from(tv.power_state),
            supports_wake: tv.supports_wake,
            is_screen_on: tv.is_screen_on,
            volume: tv.volume as i32,
            muted: tv.muted,
            backlight: tv.backlight as i32,
            current_input: SharedString::from(tv.current_input),
            available_input_ids,
            system_profile: SharedString::from(tv.system_profile),
            user_profile: SharedString::from(user_profile),
            primary_input: SharedString::from(tv.primary_input),
            managed: tv.managed,
            automated: tv.automated,
            hw_manufacturer: SharedString::from(tv.hardware_id.manufacturer.unwrap_or_default()),
            hw_model: SharedString::from(tv.hardware_id.model.unwrap_or_default()),
            hw_serial: SharedString::from(tv.hardware_id.serial.unwrap_or_default()),
            desktop_connector: SharedString::from(tv.desktop.connector),
            vendor_info: tv.vendor_info,
        });
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(StateSnapshot {
        tvs: models,
        system_profiles,
        user_profiles,
    })
}
