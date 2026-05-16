//! Daemon-side derivation of the user-facing power-state string.
//!
//! `tisvlay-core`'s [`PowerState`] is the rich, vendor-reported state
//! (`Active`, `ActiveStandby`, `ScreenOff`, `Standby`, `Suspend`,
//! `ScreenSaver`, `Unknown`). The daemon collapses this into a small
//! set of strings that appear on the D-Bus `Tv1.Power.PowerState`
//! property — `"active"`, `"screen_off"`, `"standby"` — and uses
//! the raw enum directly for reliable-dispatch pre-flight checks
//! (`screen_on` / `screen_off` need to distinguish `ActiveStandby`
//! from `ScreenOff`, which the boolean collapse would lose).
//!
//! The mapping is daemon-internal (not part of `TvState`'s public
//! surface) because it's a UX/contract decision specific to this
//! daemon: backends emit `PowerState` variants in their own
//! semantics; the daemon picks a short, stable string vocabulary for
//! its D-Bus consumers. Centralizing the mapping here avoids two
//! call sites (`state_forwarder`'s property emit and
//! `register_tv_interfaces`'s initial-state seed) drifting
//! independently — which would silently break clients that compare
//! the cached property by string.

use tisvlay_core::PowerState;

/// D-Bus property reading when the TV is past Screen Off — i.e.
/// `Standby` / `Suspend` / `ActiveStandby` / `Unknown`. Covers both
/// deep-off and the LG `ActiveStandby` transition state (NIC alive,
/// panel off, daemon can't trust it as a steady target).
pub(crate) const STANDBY: &str = "standby";

/// D-Bus property reading when the TV is on but the panel is blank
/// in the true `ScreenOff` substate — `is_on=true`, panel off, no
/// `ActiveStandby` transition pending.
pub(crate) const SCREEN_OFF: &str = "screen_off";

/// D-Bus property reading when the TV is fully usable — `Active`
/// or `ScreenSaver`. The reliable `screen_on` verify check returns
/// success on this value.
pub(crate) const ACTIVE: &str = "active";

/// Map a raw [`PowerState`] to the D-Bus `Tv1.Power.PowerState`
/// property string.
///
/// Variants that read identically on the wire (`Active` and
/// `ScreenSaver`; `Standby` / `Suspend` / `ActiveStandby` /
/// `Unknown`) all collapse to one string here — clients only ever
/// see the small vocabulary. The reliable-dispatch logic in
/// `tv_manager` matches on the raw enum, not these strings.
pub(crate) fn derive_power_state(state: PowerState) -> &'static str {
    match state {
        PowerState::Active | PowerState::ScreenSaver => ACTIVE,
        PowerState::ScreenOff => SCREEN_OFF,
        PowerState::Standby
        | PowerState::Suspend
        | PowerState::ActiveStandby
        | PowerState::Unknown => STANDBY,
    }
}

/// Whether a cached [`PowerState`] is one of the "off-ish" substates
/// that the reliable `screen_on` / `screen_off` pre-flight should
/// treat as "TV not in a usable steady state — wake it first".
///
/// Covers `Standby` / `Suspend` (truly off, NIC may be down),
/// `ActiveStandby` (transitional, NIC alive but panel dark — LG drops
/// here after remote-press power-off and can sit for tens of seconds
/// before reaching `Standby`), and `Unknown` (no push received yet,
/// safest to assume off).
///
/// `ScreenOff` deliberately returns `false`: that's the true
/// "user dispatched screen_off, panel blank, TV otherwise reachable"
/// substate and is the target steady state for the screen_off path.
pub(crate) fn needs_wake_preflight(state: PowerState) -> bool {
    matches!(
        state,
        PowerState::Standby | PowerState::Suspend | PowerState::ActiveStandby | PowerState::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_table() {
        assert_eq!(derive_power_state(PowerState::Active), ACTIVE);
        assert_eq!(derive_power_state(PowerState::ScreenSaver), ACTIVE);
        assert_eq!(derive_power_state(PowerState::ScreenOff), SCREEN_OFF);
        assert_eq!(derive_power_state(PowerState::Standby), STANDBY);
        assert_eq!(derive_power_state(PowerState::Suspend), STANDBY);
        assert_eq!(derive_power_state(PowerState::ActiveStandby), STANDBY);
        assert_eq!(derive_power_state(PowerState::Unknown), STANDBY);
    }

    /// Pins the exact D-Bus property strings — clients compare by
    /// literal, and drift between the derivation arms and downstream
    /// expectations would be silent.
    #[test]
    fn constants_match_match_arms() {
        assert_eq!(ACTIVE, "active");
        assert_eq!(SCREEN_OFF, "screen_off");
        assert_eq!(STANDBY, "standby");
    }

    /// Pins the pre-flight discriminator: `ScreenOff` is the *only*
    /// non-`Active`/`ScreenSaver` substate that should NOT trigger
    /// a wake — it's the target state for `screen_off`. Drift here
    /// breaks the idempotency of a duplicate `screen_off` on an
    /// already-screen-off TV.
    #[test]
    fn needs_wake_preflight_table() {
        assert!(!needs_wake_preflight(PowerState::Active));
        assert!(!needs_wake_preflight(PowerState::ScreenSaver));
        assert!(!needs_wake_preflight(PowerState::ScreenOff));
        assert!(needs_wake_preflight(PowerState::Standby));
        assert!(needs_wake_preflight(PowerState::Suspend));
        assert!(needs_wake_preflight(PowerState::ActiveStandby));
        assert!(needs_wake_preflight(PowerState::Unknown));
    }
}
