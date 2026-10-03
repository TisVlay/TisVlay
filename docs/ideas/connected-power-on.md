# Power-on for an already-connected TV

Every daemon start logs a warning per connected LG TV:

```
backend command failed  TV=<uuid> CMD=PowerOn ERROR=Operation not supported: power_on (requires capability: power)
```

It's existing behavior, seen during QA of PR #136. This doc records what happens and the open design question, so the fix gets chosen deliberately rather than as log cleanup.

## What happens

The default profile's `Startup` event dispatches `power_on` (with input restore) to every TV. `power_on_tv` in `crates/tisvlay-world/src/systems.rs` branches on whether the TV has a live connection:

- **Not connected, wakeable** (`Link.supports_wake`): re-dial with wake, which is WoL for LG. This is the path power-on was designed around.
- **Connected:** send `BackendCmd::PowerOn` to the backend task, which calls `TvConnection::power_on`.
- **Neither:** reply `wake-not-supported`.

LG reconnects within a second of a daemon start, so the `Startup` dispatch usually finds it connected and takes the second branch. `tisvlay-lg`'s connection never overrides `power_on`, because LG power-on only exists as WoL during a dial, so the trait default returns `Error::not_supported("power_on", POWER)`. The backend task logs it at warn (`crates/tisvlay-world/src/backend.rs`). There's no rpc reply to fail: `PowerOn` carries no reply id, and the input restore is armed separately.

CEC doesn't hit this. Its `power_on` sends Image View On over the live link (wake-in-place), which is harmless for an already-on TV.

## Why it's more than log noise

- **A capability/behavior mismatch.** LG advertises `Capabilities::STOCK`, which includes `POWER`, while its `power_on` reports `POWER` as unsupported. The error names a capability the backend claims to have. `POWER` currently means power-off and power-state reads plus "can be powered on *somehow*", where the how is the world's wake dial rather than a backend call. The model doesn't separate "power-on via dial" from "power-on via the live connection".
- **A possible dark-panel case (unverified).** An LG can stay connected with only its panel off: the screen-off substate entered by `turnOffScreen`, which the daemon itself uses for idle screen-off. If the daemon starts (or a profile fires `power_on`) while the TV is in that state, the connected branch sends a `PowerOn` that does nothing, and the panel stays dark even though the user's intent was "TV on". This follows from the code; it hasn't been reproduced. Reproduce first: blank the panel (`tisvlay set-screen <uuid> off` or idle screen-off), restart the daemon, and see whether the TV lights.

## The open question

What does `power_on` mean for a TV that's already connected? The answer picks the fix:

- **"Make the TV usable" (power + panel on).** For LG, connected implies powered: it drops the WebSocket in standby. So power-on on a live link reduces to `screen_on`.
- **"Power the device" (strictly power).** For a connected LG that's already done, so power-on is a satisfied no-op, and the panel is a separate `screen_on` concern for the profile to request.

The profile vocabulary already has distinct `screen_on` and `power_on` actions. Whether `power_on` should imply the panel is a policy question, which matters given the config-facts / profile-policy split.

## Options

1. **LG implements `power_on` as `screen_on` on a live link.** It reuses `client.screen_on()`, which already treats the TV's "already on" error as success. Fixes the dark-panel case (if real), silences the warning, and matches CEC's wake-in-place idempotence. Bakes the "make it usable" meaning into one backend; the same decision then has to be made for every future backend.
2. **The world treats `PowerOn` on a connected TV as satisfied.** Skip the backend call in the connected branch, maybe only for backends without wake-in-place. Vendor-agnostic and quiet, but drops CEC's useful Image View On (a CEC TV can be connected *and* in standby; connected ≠ powered there) unless it's split by a new trait flag. Leaves the dark-panel case unfixed.
3. **Split the capability.** Distinguish power-on via the live connection (CEC) from power-on via a wake dial (LG WoL), either as a capability bit or a connector trait method. The world then routes `PowerOn` for a connected TV to `power_on` only where the backend supports it, and otherwise to the policy answer (no-op or `screen_on`). Most explicit and fixes the misleading error, at the cost of touching core traits, the world and both backends.
4. **Demote the log to debug.** Hides the symptom and fixes neither the mismatch nor the dark-panel case. Only worth it as a stopgap.

## Trigger conditions

- The dark-panel case reproduces: a connected-but-screen-off LG stays dark after a daemon restart or a `power_on` profile action.
- A third backend lands and needs its own answer for connected power-on.
- The warning becomes a support question ("is this error a problem?").

## Related

- [Richer `DispatchOutcome` return type](dispatch-outcome-return-type.md): another place where a bare success hides which path was taken.
- [Event-driven power-management redesign](event-driven-power-management.md).
- `docs/cec/protocol.md`: connected ≠ powered for CEC, the reason option 2 can't be applied blindly.
