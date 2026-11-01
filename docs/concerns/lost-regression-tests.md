## Two regression tests didn't survive the 0.3.0 rewrite

**Priority**: low
**Found by**: the 2026-11 docs sweep, re-pointing the regression-coverage notes at today's tests

### Context

The rewrite to the zenoh and bevy_ecs stack replaced the old daemon's tests with the world and wire suites. Two behaviors the old tests pinned have no successor:

- **CEC identity follows a dongle swap.** `hardware_id_refreshes_on_reconnect_to_a_different_device` checked that reconnecting to a different device republishes its make and model. Today the identity is captured on connect (`backend.rs` in `tisvlay-world`) and published as `meta/identity`, but no test swaps the device under a live TV. ([cec/testing.md](../cec/testing.md))
- **LG `screen_off` accepts `ActiveStandby` when the wake refuses.** `screen_off_accepts_active_standby_when_wake_refuses` pinned the graceful path for a TV whose firmware latches against `turnOn` after a remote-off. The world suite pins the neighboring case, a TV that keeps ACKing dark (`escalation_reasserts_on_reconnect_then_gives_up`), but not this one. ([lg/firmware-quirks.md](../lg/firmware-quirks.md))

### Risk

Both are field-found behaviors. A regression would show up as stale make and model after swapping a CEC adapter, or as a failed `screen_off` reported for an LG TV that did blank.

### Mitigation

Add a world test for each, driving the mock: a reconnect whose connection reports a different identity, and a pre-flight wake that never leaves `ActiveStandby` (the mock's `set_initial_power_state_override` already supports it).

### Trigger

Any change to identity capture or to the screen dispatch's pre-flight wake.
