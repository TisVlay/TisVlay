//! Cached TV state for tray menu rendering.
//!
//! Builds snapshots from `TvInfo` (from `list_tvs()` via `GetManagedObjects`)
//! which includes both identity and cached state properties.

use tisvlay_core::{DEFAULT_USER_PROFILE_UUID, DaemonClient, SessionClient};
use tracing::debug;

/// Snapshot of a single TV's state, cached locally for menu rendering.
///
/// Profile fields carry UUIDs on the wire (identity) and `(uuid, name)`
/// pairs in the catalog (identity + display). `Tv1.SystemProfile` and
/// `Session1.GetTvUserProfile` return UUIDs, and the matching setters
/// reject anything that isn't a known UUID with `InvalidArgs` — so
/// dispatch must thread the UUID through, but display has to look up
/// the name in the catalog for the user-facing label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CachedTvState {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub is_on: bool,
    pub is_screen_on: bool,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub current_input: Option<String>,
    pub backlight: Option<u8>,
    /// Available inputs as `(id, label)` pairs.
    pub inputs: Vec<(String, String)>,
    /// UUID of the user profile assigned to this TV, from
    /// `Session1.GetTvUserProfile`. `None` when the session service is
    /// unavailable or the per-TV fetch failed.
    pub user_profile_uuid: Option<String>,
    /// User-profile catalog as `(uuid, name)` pairs, sorted reserved-
    /// default first then alphabetical by name.
    pub available_user_profiles: Vec<(String, String)>,
    /// UUID of the system profile assigned to this TV. Empty assignment
    /// collapses to `DEFAULT_SYSTEM_PROFILE_UUID` at TV-config load
    /// time, so this is always a real UUID on the wire.
    pub system_profile_uuid: String,
    /// System-profile catalog as `(uuid, name)` pairs, same sort.
    pub available_system_profiles: Vec<(String, String)>,
    /// Whether this TV participates in automated management.
    pub managed: bool,
    /// Whether automation should fire for this TV right now.
    pub automated: bool,
}

/// Fetch current state for all configured TVs.
///
/// Uses a single `GetManagedObjects` call (via `list_tvs()`) to retrieve
/// both identity and cached state properties for all TVs. No per-TV
/// round-trips or connection attempts are made.
///
/// Optionally queries the session service for user profile info.
pub(crate) async fn fetch_all_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> anyhow::Result<Vec<CachedTvState>> {
    let tvs = daemon.list_tvs().await?;
    for tv in &tvs {
        debug!(tv_id = %tv.uuid, connected = tv.connected, "TV from daemon");
    }

    // Fetch user-profile catalog from session service (once, shared
    // across TVs). The body-wrapper returns `HashMap<Uuid, Body>`;
    // re-shape into the `(uuid, name)` pair the tray menu needs and
    // sort with the reserved-default UUID first then alphabetically
    // by name — same order the GUI's `profile_name_list` uses, so
    // the two clients render the same picker.
    let available_user_profiles: Vec<(String, String)> = match session {
        Some(session_client) => match session_client.list_user_profile_bodies().await {
            Ok(bodies) => sort_profile_catalog(
                bodies
                    .into_iter()
                    .map(|(u, b)| (u.to_string(), b.name))
                    .collect(),
                DEFAULT_USER_PROFILE_UUID.to_string(),
            ),
            Err(e) => {
                debug!(error = %e, "Failed to list user profile bodies from session service");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    // Fetch system-profile catalog from daemon (same shape + sort).
    let default_system_uuid = tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID.to_string();
    let available_system_profiles: Vec<(String, String)> =
        match daemon.list_system_profile_bodies().await {
            Ok(bodies) => sort_profile_catalog(
                bodies
                    .into_iter()
                    .map(|(u, b)| (u.to_string(), b.name))
                    .collect(),
                default_system_uuid.clone(),
            ),
            Err(e) => {
                debug!(error = %e, "Failed to list system profile bodies from daemon");
                Vec::new()
            }
        };

    let mut states = Vec::with_capacity(tvs.len());

    for tv in tvs {
        // Fetch per-TV user profile assignment (UUID) from session
        // service.
        let user_profile_uuid = match session {
            Some(session_client) => session_client.get_tv_user_profile(&tv.uuid).await.ok(),
            None => None,
        };

        let is_on = tv.power_state != "standby" && tv.power_state != "unknown";

        let cached = CachedTvState {
            id: tv.uuid.clone(),
            name: tv.name,
            connected: tv.connected,
            is_on,
            is_screen_on: tv.is_screen_on,
            volume: if tv.connected { Some(tv.volume) } else { None },
            muted: if tv.connected { Some(tv.muted) } else { None },
            current_input: if tv.current_input.is_empty() {
                None
            } else {
                Some(tv.current_input)
            },
            backlight: if tv.connected {
                Some(tv.backlight)
            } else {
                None
            },
            inputs: tv.inputs,
            user_profile_uuid,
            available_user_profiles: available_user_profiles.clone(),
            system_profile_uuid: tv.system_profile,
            available_system_profiles: available_system_profiles.clone(),
            managed: tv.managed,
            automated: tv.automated,
        };

        debug!(
            tv_id = %cached.id,
            connected = cached.connected,
            is_on = cached.is_on,
            volume = ?cached.volume,
            muted = ?cached.muted,
            "Fetched TV state"
        );
        states.push(cached);
    }

    states.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(states)
}

/// Sort a profile catalog so the reserved-default UUID appears first,
/// then alphabetically by friendly name. Mirrors the GUI's
/// `state.rs::profile_name_list` order so both clients render the
/// same picker.
fn sort_profile_catalog(
    mut entries: Vec<(String, String)>,
    default_uuid: String,
) -> Vec<(String, String)> {
    entries.sort_by(|a, b| {
        let a_default = a.0 == default_uuid;
        let b_default = b.0 == default_uuid;
        match (a_default, b_default) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.1.cmp(&b.1),
        }
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture cache. `system_profiles` / `user_profiles` are
    /// `(uuid, name)` pairs — the same shape `fetch_all_state`
    /// produces.
    fn make_tv_state(
        system_profiles: Vec<(&str, &str)>,
        user_profiles: Vec<(&str, &str)>,
    ) -> CachedTvState {
        CachedTvState {
            id: "test".into(),
            name: "Test".into(),
            connected: true,
            is_on: true,
            is_screen_on: true,
            volume: Some(50),
            muted: Some(false),
            current_input: Some("HDMI_1".into()),
            backlight: Some(80),
            inputs: vec![("HDMI_1".into(), "HDMI 1".into())],
            user_profile_uuid: Some(DEFAULT_USER_PROFILE_UUID.to_string()),
            available_user_profiles: user_profiles
                .into_iter()
                .map(|(u, n)| (u.to_string(), n.to_string()))
                .collect(),
            system_profile_uuid: tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID.to_string(),
            available_system_profiles: system_profiles
                .into_iter()
                .map(|(u, n)| (u.to_string(), n.to_string()))
                .collect(),
            managed: true,
            automated: true,
        }
    }

    #[test]
    fn test_cached_tv_state_equality_same_data() {
        let a = make_tv_state(
            vec![("uuid-sys-1", "default"), ("uuid-sys-2", "guest")],
            vec![("uuid-usr-1", "default"), ("uuid-usr-2", "gaming")],
        );
        let b = make_tv_state(
            vec![("uuid-sys-1", "default"), ("uuid-sys-2", "guest")],
            vec![("uuid-usr-1", "default"), ("uuid-usr-2", "gaming")],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn test_cached_tv_state_inequality_different_data() {
        let a = make_tv_state(vec![("uuid-sys", "default")], vec![("uuid-usr", "default")]);
        let mut b = a.clone();
        b.volume = Some(51);
        assert_ne!(a, b);
    }

    #[test]
    fn test_cached_tv_state_profile_order_matters_for_equality() {
        // Profile catalogs are Vec<(uuid, name)>, so order matters
        // for PartialEq. `fetch_all_state` runs them through
        // `sort_profile_catalog` (default UUID first, then
        // alphabetical), giving the tray a stable order across
        // refreshes.
        let a = make_tv_state(
            vec![("uuid-sys-1", "default"), ("uuid-sys-2", "guest")],
            vec![("uuid-usr-1", "default"), ("uuid-usr-2", "gaming")],
        );
        let b = make_tv_state(
            vec![("uuid-sys-1", "default"), ("uuid-sys-2", "guest")],
            vec![("uuid-usr-1", "default"), ("uuid-usr-2", "gaming")],
        );
        assert_eq!(a, b);

        // Reversed order would compare unequal — the catalog sort
        // prevents this drift between two consecutive fetches.
        let c = make_tv_state(
            vec![("uuid-sys-2", "guest"), ("uuid-sys-1", "default")],
            vec![("uuid-usr-2", "gaming"), ("uuid-usr-1", "default")],
        );
        assert_ne!(a, c);
    }

    #[test]
    fn sort_profile_catalog_default_first_then_alpha() {
        let default_uuid = "uuid-default".to_string();
        let entries = vec![
            ("uuid-other".to_string(), "zulu".to_string()),
            ("uuid-default".to_string(), "default".to_string()),
            ("uuid-alpha".to_string(), "alpha".to_string()),
        ];
        let sorted = sort_profile_catalog(entries, default_uuid);
        assert_eq!(
            sorted,
            vec![
                ("uuid-default".to_string(), "default".to_string()),
                ("uuid-alpha".to_string(), "alpha".to_string()),
                ("uuid-other".to_string(), "zulu".to_string()),
            ]
        );
    }
}
