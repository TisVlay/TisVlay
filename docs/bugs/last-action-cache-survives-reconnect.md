## Session event handler `last_action` cache survives daemon reconnects

**Status**: open
**Severity**: low — can cause a missed action after daemon restart
**Found by**: session event test design review

### Problem

The session service's event handler tracks the last action sent to
each TV in a `HashMap<String, SessionAction>` to skip redundant
commands. This cache is never invalidated when the daemon disconnects
and reconnects.

If the daemon restarts (or the TV reconnects with a fresh state), the
cache still thinks `ScreenOff` was the last applied action. The next
`SessionLocked` event is skipped as redundant, even though the TV's
screen is actually on after the restart.

### Fix plan

Two options with different granularity:

1. **Coarse (simpler)**: clear the entire `last_action` map on
   `StateEvent::DaemonDisappeared`. Clears all TVs even if only one
   reconnected, but daemon restarts are rare and the cost of
   re-sending one redundant `screen_off` is negligible.

2. **Per-TV (more precise)**: clear a single TV's entry when
   `PropertiesChanged { tv_id }` indicates `Connected` transitioned
   to `true` (fresh connection). The session service already
   subscribes to `StateEvent::DaemonStateChanged` which includes
   `PropertiesChanged { tv_id }` — it would need to check if the
   changed property is `Connected`.

Option 1 is sufficient for the current scale. Option 2 is better if
the project adds many TVs where one reconnects frequently while
others are stable.

### Affected files

- `crates/tisvlay-session/src/lib.rs` — `event_handler()` loop
