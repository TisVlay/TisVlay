//! Cached TV state for tray menu rendering.
//!
//! Combines `TvInfo` (from `list_tvs()`) with `TvState` (from `get_state()`)
//! into a single snapshot used by the tray module to build menus.

use tisvlay_core::{DaemonClient, SessionClient};
use tracing::debug;

/// Snapshot of a single TV's state, cached locally for menu rendering.
#[derive(Debug, Clone)]
pub struct CachedTvState {
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
    /// Current profile name assigned to this TV.
    pub profile: Option<String>,
    /// Available profile names.
    pub available_profiles: Vec<String>,
}

/// Fetch current state for all configured TVs.
///
/// Calls `list_tvs()` to get the TV list, then `get_state()` for each
/// connected TV. Optionally queries the session service for profile info.
pub async fn fetch_all_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> anyhow::Result<Vec<CachedTvState>> {
    let tvs = daemon.list_tvs().await?;
    for tv in &tvs {
        debug!(tv_id = %tv.id, connected = tv.connected, "TV from daemon");
    }

    // Fetch available profiles from session service (once, shared across TVs)
    let available_profiles: Vec<String> = match session {
        Some(session_client) => match session_client.list_profiles().await {
            Ok(profiles) => profiles.into_iter().map(|(name, _)| name).collect(),
            Err(e) => {
                debug!(error = %e, "Failed to list profiles from session service");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    let mut states = Vec::with_capacity(tvs.len());

    for tv in tvs {
        // Fetch per-TV profile assignment from session service
        let profile = match session {
            Some(session_client) => session_client.get_tv_profile(&tv.id).await.ok(),
            None => None,
        };

        let mut cached = CachedTvState {
            id: tv.id.clone(),
            name: tv.name,
            connected: tv.connected,
            is_on: false,
            is_screen_on: false,
            volume: None,
            muted: None,
            current_input: None,
            backlight: None,
            inputs: Vec::new(),
            profile,
            available_profiles: available_profiles.clone(),
        };

        // Try get_state for all TVs — the daemon connects on-demand, so a
        // TV may be reachable even if `Connected` is false. Use a short
        // timeout for disconnected TVs to avoid blocking the poll on
        // WebSocket connect timeouts to powered-off TVs.
        let timeout = if tv.connected {
            std::time::Duration::from_secs(10)
        } else {
            std::time::Duration::from_secs(2)
        };
        match tokio::time::timeout(timeout, daemon.get_state(&tv.id)).await {
            Ok(Ok(state)) => {
                cached.connected = true;
                cached.is_on = state.is_on;
                cached.is_screen_on = state.is_screen_on;
                cached.volume = state.volume;
                cached.muted = state.muted;
                cached.current_input = state.current_input;
                cached.backlight = state.backlight;
                cached.inputs = state.inputs.into_iter().map(|i| (i.id, i.label)).collect();
            }
            Ok(Err(e)) => {
                debug!(tv_id = %tv.id, error = %e, "TV state unavailable");
                cached.connected = false;
            }
            Err(_) => {
                debug!(tv_id = %tv.id, "TV state timed out");
                cached.connected = false;
            }
        }

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

    Ok(states)
}
