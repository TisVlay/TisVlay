//! Cached TV state for tray menu rendering.
//!
//! Combines `TvInfo` (from `list_tvs()`) with `TvState` (from `get_state()`)
//! into a single snapshot used by the tray module to build menus.

use tisvlay_core::DaemonClient;
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
}

/// Fetch current state for all configured TVs.
///
/// Calls `list_tvs()` to get the TV list, then `get_state()` for each
/// connected TV. Disconnected TVs get default state values.
pub async fn fetch_all_state(daemon: &DaemonClient) -> anyhow::Result<Vec<CachedTvState>> {
    let tvs = daemon.list_tvs().await?;
    for tv in &tvs {
        debug!(tv_id = %tv.id, connected = tv.connected, "TV from daemon");
    }
    let mut states = Vec::with_capacity(tvs.len());

    for tv in tvs {
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
        };

        // Always try get_state — the daemon connects on-demand, so a TV
        // may be reachable even if `Connected` is false. If get_state
        // succeeds, the daemon established a connection and the TV is online.
        match daemon.get_state(&tv.id).await {
            Ok(state) => {
                cached.connected = true;
                cached.is_on = state.is_on;
                cached.is_screen_on = state.is_screen_on;
                cached.volume = state.volume;
                cached.muted = state.muted;
                cached.current_input = state.current_input;
                cached.backlight = state.backlight;
                cached.inputs = state.inputs.into_iter().map(|i| (i.id, i.label)).collect();
            }
            Err(e) => {
                debug!(tv_id = %tv.id, error = %e, "TV state unavailable");
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
