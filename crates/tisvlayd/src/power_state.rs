//! Daemon-side derivation of the user-facing power-state string.
//!
//! `tisvlay-core`'s `PowerState` is the rich, vendor-reported state
//! (`Active`, `ActiveStandby`, `ScreenOff`, `Standby`, `Suspend`,
//! `ScreenSaver`, `Unknown`). The daemon collapses this — via
//! [`TvState::update_power`](tisvlay_core::TvState::update_power),
//! which keeps `is_on` and `is_screen_on` as the canonical booleans —
//! into a small set of strings that appear on the D-Bus
//! `Tv1.Power.PowerState` property and in `ManagedTv::last_known_power_state`.
//!
//! The mapping is daemon-internal (not part of `TvState`'s public
//! surface) because it's a UX/contract decision specific to this
//! daemon: backends emit `PowerState` variants in their own
//! semantics; the daemon picks a short, stable string vocabulary for
//! its consumers. Centralizing the mapping here avoids three call
//! sites (`state_forwarder`, `run_subscription_task`'s cached-state
//! write, and `register_tv_interfaces`'s initial-state seed) drifting
//! independently — which would silently break the reliable-tier
//! short-circuit that reads the same field by string comparison.

use tisvlay_core::TvState;

/// Cached `PowerState` reading when the TV is past Screen Off — i.e.
/// `state.is_on == false`, covering the underlying `Standby`,
/// `Suspend`, `PowerOff`, and `Unknown` substates. The reliable-tier
/// dispatch helper short-circuits to `power_on` on this value.
pub(crate) const STANDBY: &str = "standby";

/// Cached `PowerState` reading when the TV is on but the panel is
/// blank — `state.is_on == true && state.is_screen_on == false`,
/// covering `ScreenOff` and `ActiveStandby` (where the TV's NIC
/// stays alive and SSAP keeps responding, but the panel is dark).
pub(crate) const SCREEN_OFF: &str = "screen_off";

/// Cached `PowerState` reading when both `is_on` and `is_screen_on`
/// are true — the TV is fully usable. The reliable-tier verify check
/// returns success on this value.
pub(crate) const ACTIVE: &str = "active";

/// Map `(state.is_on, state.is_screen_on)` to the cached-property
/// string used on D-Bus and in `ManagedTv::last_known_power_state`.
///
/// Single source of truth — every site that derives this mapping
/// must route through here. The reliable-tier dispatch's
/// short-circuit and verify checks compare against the constants
/// exported above; changing either the arms here or those constants
/// without keeping them in sync will silently break the short-circuit.
pub(crate) fn derive_power_state(state: &TvState) -> &'static str {
    match (state.is_on, state.is_screen_on) {
        (false, _) => STANDBY,
        (true, false) => SCREEN_OFF,
        (true, true) => ACTIVE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::TvState;

    fn state(is_on: bool, is_screen_on: bool) -> TvState {
        let mut s = TvState::new();
        s.is_on = is_on;
        s.is_screen_on = is_screen_on;
        s
    }

    #[test]
    fn derivation_table() {
        assert_eq!(derive_power_state(&state(true, true)), ACTIVE);
        assert_eq!(derive_power_state(&state(true, false)), SCREEN_OFF);
        assert_eq!(derive_power_state(&state(false, true)), STANDBY);
        assert_eq!(derive_power_state(&state(false, false)), STANDBY);
    }

    #[test]
    fn constants_match_match_arms() {
        // Pin the exact string values to prevent drift between the
        // derivation site and downstream comparisons (e.g. the
        // reliable-tier short-circuit reading
        // `last_known_power_state` against `STANDBY`).
        assert_eq!(ACTIVE, "active");
        assert_eq!(SCREEN_OFF, "screen_off");
        assert_eq!(STANDBY, "standby");
    }
}
