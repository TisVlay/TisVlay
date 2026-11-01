## A poll-only network backend can't confirm its screen actions

**Priority**: low (no such backend exists today)
**Found by**: the 2026-11 docs sweep, correcting the vendor guide's state contract

### Context

The reliable screen dispatch (`DispatchVerify` in `tisvlay-world`'s `systems/dispatch.rs`) checks the TV's cached power state after the backend's verify delay, retries, then gives up: an unmet `screen_on` escalates to a wake, an unmet `screen_off` publishes `event/dispatch-failed`. Three things write that cache:

- the state subscription of a push backend (LG);
- a `get_state()` the daemon runs after each power or screen command, only for a `reachable_in_standby` backend (CEC);
- the snapshot taken on connect.

A network backend that can't push state and isn't reachable in standby gets none of these between connects. Every screen action it takes then reads as unconfirmed: a `screen_on` that worked still escalates to a wake, and a `screen_off` that worked still reports failure. The vendor guide ([implementing-a-vendor.md § What the verify loop reads](../backends/implementing-a-vendor.md#what-the-verify-loop-reads)) says so; the code has no answer yet.

### Risk

The first poll-only backend (an RS-232 or HTTP-polled set, say) would misbehave on every screen action, in a way that looks like a firmware problem.

### Mitigation options

- **Refresh after commands for every backend without a subscription**, not only `reachable_in_standby` ones: the `get_state()` after a power or screen command in `backend.rs` would key on "no subscription" instead.
- **A periodic poll task** for backends without a subscription, at a backend-chosen interval.
- **Require `subscribe_state()`** of network backends, and say so in the guide.

### Trigger

A poll-only backend is proposed.
