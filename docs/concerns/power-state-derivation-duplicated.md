# `(is_on, is_screen_on) → "..."` derivation duplicated across three call sites

## Context

The daemon converts `(TvState::is_on, TvState::is_screen_on)` into a short string (`"active"` / `"screen_off"` / `"standby"`) at three call sites:

- [`crates/tisvlayd/src/state_forwarder.rs`](../../crates/tisvlayd/src/state_forwarder.rs) — emits the D-Bus `Tv1.Power.PowerState` property.
- [`crates/tisvlayd/src/tv_manager.rs::run_subscription_task`](../../crates/tisvlayd/src/tv_manager.rs) — writes `ManagedTv::last_known_power_state` on every subscription push.
- [`crates/tisvlayd/src/dbus/tv.rs`](../../crates/tisvlayd/src/dbus/tv.rs) — the initial-state branch of `register_tv_interfaces` seeds the cached property at connection time.

The same `match (state.is_on, state.is_screen_on)` arms (`(false, _) => "standby"`, `(true, false) => "screen_off"`, `(true, true) => "active"`) live in three places, manually kept in sync.

## Risk

`dispatch_reliable_screen_on` compares cached state against the bare string `"standby"` ([tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs) around line 1733). The pre-flight short-circuit, the verify-retry loop's success check (`cached == "active"`), and the `register_tv_interfaces` initial-state seed all assume the three derivation sites produce identical strings. Any future change to the mapping that lands in one call site but misses another silently breaks the short-circuit — and there's no compile-time linkage between the string literals and the derivation logic.

`last_known_power_state: String` on `ManagedTv` is also paying the storage cost of a small set of distinct values, which would shrink to an enum byte.

## Mitigation options

1. **Extract a single helper** — `derive_power_state_str(state: &TvState) -> &'static str` in `tisvlay-core::state` or `tisvlayd::state_forwarder`. All three call sites call it; the short-circuit comparison uses a `pub const STANDBY: &str = "standby";` (or similar) exported from the same module. Single source of truth; the linkage is implicit but at least the strings can't drift.

2. **Promote to an enum** — `pub enum DerivedPowerState { Standby, ScreenOff, Active }` with `From<&TvState>`. All three derivation sites build the enum; the D-Bus serialization happens at the property-emission edge only. The short-circuit comparison becomes `derived == DerivedPowerState::Standby`. Strongest compile-time guarantee, but adds a type that's basically a small subset of `PowerState`.

3. **Inline-document the synchronization invariant** at each derivation site so a future contributor changing one is more likely to find the others. Cheapest but most brittle.

## Trigger to revisit

- A regression where the short-circuit silently fails to fire (or fires spuriously) because one of the three derivations changed.
- A new `(is_on, is_screen_on)` arm gets added (e.g., a new TV substate from a different vendor backend).
- The `last_known_power_state: String` field shows up as a perf hot-spot (unlikely — sub-second cadence — but worth noting).

## Priority

Low. Surface area is the daemon's internal power-state derivation; both consumers are in the same crate and visible to any contributor working on `dispatch_reliable_screen_on`. Flagged by Copilot on PR #56 review.

## Related

- [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md) — where the pre-flight short-circuit's design rationale lives.
- [docs/design.md](../design.md) "Action QoS (Reliability)" — top-level architecture for the reliable-tier dispatch.
