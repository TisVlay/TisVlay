# GUI and tray show a remote-off LG TV as powered on

**Status:** open.

After the user turns an LG TV off with its remote, the TV reports `activestandby` for a while before settling into deep standby: the network card stays up, the panel is dark and the red standby LED is on (see [LG firmware quirks](../lg/firmware-quirks.md)). `PowerState::is_on` counts `ActiveStandby` as on, and both clients build their Power state from it:

- GUI: `is_on: tv.connected && tv.power_state.is_on()` in `crates/tisvlay-gui-app/src/state.rs`.
- Tray: `tv.connected && tv.is_on` in `crates/tisvlay-tray/src/tray.rs`, where `is_on` is the `state/power` payload's field, derived from the same classification.

So the GUI's Power switch and the tray read *on* for a TV the user can see is off, and flipping the switch sends power-off to a TV that's already dark; the user has to act twice to wake it.

The CLI's `power` getter and `power --toggle` use the daemon's wake check instead: on means connected and `!PowerState::needs_wake_preflight()` (active, screensaver, screen-off), so a remote-off LG reads `off` and a toggle wakes it.

## Fix direction

Move the GUI and tray to the same rule, so all three clients agree. Either share one helper (a `PowerState` method naming "awake", used by all three) or change what `state/power.is_on` means; the latter is a wire-visible change (a `feat`, not a `fix`, per the commit rules) and touches every consumer of the payload, so the shared helper is the narrower route. Whichever way, extend the existing table test in `tisvlay-core`'s `state.rs` so each `PowerState` variant's verdict is pinned.

History: [power-state-vocabulary-drift.md](power-state-vocabulary-drift.md) closed an earlier version of this, where `"activestandby"` read as on because of a stale string blocklist; the fix delegated to `PowerState::is_on`, which keeps `ActiveStandby` on.
