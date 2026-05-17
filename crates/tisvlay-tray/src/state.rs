//! Cached TV state for tray menu rendering.
//!
//! Builds snapshots from `TvInfo` (from `list_tvs()` via `GetManagedObjects`)
//! which includes both identity and cached state properties.

use tisvlay_core::{DaemonClient, SessionClient};
use tracing::debug;

/// Snapshot of a single TV's state, cached locally for menu rendering.
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
    /// Current user profile UUID assigned to this TV (string-formatted).
    pub user_profile_uuid: Option<String>,
    /// Available user profiles as `(uuid_string, name)` pairs. The tray
    /// renders `name` in the radio menu and passes `uuid` through
    /// `Session1.SetTvUserProfile` on click.
    pub available_user_profiles: Vec<(String, String)>,
    /// Current system profile UUID assigned to this TV (string-formatted).
    pub system_profile_uuid: String,
    /// Available system profiles as `(uuid_string, name)` pairs.
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

    // Fetch available user profiles as (uuid_string, name) pairs from
    // the session service's per-profile D-Bus objects. The menu shows
    // `name` but selection passes `uuid` to `SetTvUserProfile`.
    let available_user_profiles: Vec<(String, String)> = match session {
        Some(session_client) => match session_client.list_user_profiles_with_ids().await {
            Ok(profiles) => profiles,
            Err(e) => {
                debug!(error = %e, "Failed to list user profiles from session service");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    // Fetch available system profiles from daemon as (uuid_string, name) pairs.
    let available_system_profiles: Vec<(String, String)> =
        match daemon.list_system_profiles_with_ids().await {
            Ok(profiles) => profiles,
            Err(e) => {
                debug!(error = %e, "Failed to list system profiles from daemon");
                Vec::new()
            }
        };

    let mut states = Vec::with_capacity(tvs.len());

    for tv in tvs {
        // Fetch per-TV user profile UUID from session service (string-formatted).
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

#[cfg(test)]
mod tests {
    use super::*;

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
            user_profile_uuid: Some(tisvlay_core::DEFAULT_USER_PROFILE_UUID.to_string()),
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
            vec![("u1", "default"), ("u2", "guest")],
            vec![("u3", "default"), ("u4", "gaming")],
        );
        let b = make_tv_state(
            vec![("u1", "default"), ("u2", "guest")],
            vec![("u3", "default"), ("u4", "gaming")],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn test_cached_tv_state_inequality_different_data() {
        let a = make_tv_state(vec![("u1", "default")], vec![("u3", "default")]);
        let mut b = a.clone();
        b.volume = Some(51);
        assert_ne!(a, b);
    }

    #[test]
    fn test_cached_tv_state_profile_order_matters_for_equality() {
        let a = make_tv_state(
            vec![("u1", "default"), ("u2", "guest")],
            vec![("u3", "default"), ("u4", "gaming")],
        );
        let b = make_tv_state(
            vec![("u1", "default"), ("u2", "guest")],
            vec![("u3", "default"), ("u4", "gaming")],
        );
        assert_eq!(a, b);

        // Different ordering would be unequal.
        let c = make_tv_state(
            vec![("u2", "guest"), ("u1", "default")],
            vec![("u4", "gaming"), ("u3", "default")],
        );
        assert_ne!(a, c);
    }
}
