# CEC screen-via-power emulation

A per-device opt-out CEC config that makes screen-blank profile actions (`screen_off` / `screen_on`) power the TV off/on, so the **shipped default profiles behave consistently across a mixed fleet** without the user rewriting them to power actions for a CEC-only setup. HDMI-CEC has no discrete screen-off (the panel can't blank while the TV stays "on"), so a device with `POWER` but no `SCREEN` otherwise ignores the stock screen events entirely.

> Status: **built** on `feat/cec-backend` (PR [#94](https://github.com/ruffsl/TisVlay/pull/94)), building on the reachable-in-standby decouple + wake-in-place already landed. Green: build · clippy `-D warnings` · 603 tests.

## As built (deviation from the plan below)

The plan's § Decision put the emulation **inside `CecConnection`** (`screen_off`→`self.power_off()`). Implementation surfaced a correctness flaw that moved it up a layer:

- **The emulation lives at the daemon dispatch, not the connection.** Routing `screen_off`→`power_off` *inside* the connection bypasses `TvManager::power_off`, which is the only thing that **synthesizes** the standby cache update for a *commanded* standby (a CEC TV doesn't re-broadcast a `<Standby>` it was told to do, so the monitor never pushes it). The connection-level route left the cached power state stale-`Active` after a screen-off — which would make a later `power_on` wrongly hit its idempotent no-op and *not* wake the TV. So the mapping is done at `TvManager::screen_off`/`screen_on` (delegate to `power_off`/`power_on`) via a config-aware `TvConnector::emulate_screen_via_power()` predicate (mirrors `reachable_in_standby` / `supports_wake` — vendor-blind, the daemon asks the backend). This reuses the correct reachable-in-standby power path (keep connection, synthesize cache, wake in place).
- **`SCREEN` is added by the daemon's config-aware `get_capabilities`, not the connection.** Because it's config-driven rather than connection-state-driven, the bit is stable across connect/disconnect (no cap flicker — the property `get_capabilities` already guards against). `CecConnection::capabilities()` stays `POWER | INPUT`; `CecBackend` static caps unchanged.
- **`PairingFieldType::Bool` already existed** (the plan's Open question), so the schema field is a proper editable `Bool` (`UserInput`), default `"true"` — no core field-type addition needed.
- **Mock parity added** (`MockVendorConfig::emulate_screen_via_power` + connector override + `mock_tv_config_screen_via_power` harness builder) so the daemon dispatch is covered end-to-end (`screen_off` action → standby, connection kept, cache accurate; `SCREEN` advertised).

Durable content migrated to [docs/cec/protocol.md](../cec/protocol.md) § Screen-via-power emulation and the vendor guide's "Emulating a capability the device lacks"; this plan deletes with the CEC PR.

## Problem

The stock profiles map presence events to *screen* actions, not power: the system default's `on_dpms_off` / `on_idle_timeout` and the user default's `on_session_lock` / `on_idle_timeout` are all `ScreenOff` (mirrors for the resume side are `ScreenOn`) — see [`config.rs`](../../crates/tisvlay-core/src/config.rs). On an LG TV those blank the OLED. CEC declares `POWER | INPUT` and no `SCREEN`, so today those actions hit `NotSupported` and no-op — a CEC-only user must hand-edit every profile to power actions (as in the dev `cec` session profile) to get any idle/lock behavior, and a mixed LG+CEC fleet behaves inconsistently under one shared profile.

## Decision

Add a **CEC vendor-config bool `emulate_screen_via_power` (default `true`)**, emulated entirely inside `CecConnection`:

- `capabilities()` → `POWER | INPUT | (SCREEN when the flag is on)`.
- `screen_off()` → `self.power_off()` (→ standby; stays connected via the decouple).
- `screen_on()` → `self.power_on()` (→ wake-in-place; no `Connected` flap, idempotent).

The daemon stays vendor-blind — it dispatches `screen_off` and the CEC connection decides how to honor it. This is the [vendor guide](../backends/implementing-a-vendor.md)'s "Emulating a capability the device lacks" pattern (mute-via-volume), applied to SCREEN.

### Why these choices

- **Config, default-on** — matches the project's "events map to configurable actions, never hardcoded" principle. Default-on makes a screen-blank event make *every* device go dark using its available primitive (LG blanks the panel, CEC powers off), so the stock profiles are uniform across a fleet. Opt-out (`false`) preserves the sub-stock-fidelity path for a user who'd rather not power-cycle a slow-to-wake panel on every idle blank — the CEC device then simply ignores screen actions.
- **Connection-level, not static-backend or daemon** — `ManagedTv::capabilities()` ([tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs)) already returns the *live connection's* capabilities, so the connection can advertise `SCREEN` conditionally on the parsed config with zero daemon or `TvBackend` changes.
- **Name** — `emulate_screen_via_power` reuses the guide's "emulate a capability" vocabulary. (Alternatives weighed: `screen_falls_back_to_power`, `screen_maps_to_power`.)

### Accepted side effect

Advertising `SCREEN` also makes the GUI/tray render a **Screen on/off toggle** for the CEC TV that power-cycles. Accepted as consistent (the config says "screen means power here") rather than hiding it and routing only profile actions — a hidden toggle whose action still fires elsewhere is more surprising.

## Composition with what's landed

- `screen_off` → `power_off`: standby, connection kept (reachable-in-standby decouple). The CLAUDE.md double-dispatch note stays safe — `power_off`-to-standby is idempotent.
- `screen_on` → `power_on` → wake-in-place: no teardown, no `Connected` flap, and the idempotent fast path makes a repeat `screen_on` on an already-`Active` TV a no-op.

## Slices

1. **Config + emulation.** `CecVendorConfig.emulate_screen_via_power: bool` (serde default `true`); `CecConnection` stores it and wires `capabilities()` / `screen_off()` / `screen_on()`; add the `config_schema()` field (`Diagnostic` or a boolean `UserInput` — see Open question) with helper copy. Ship scaffolding + wiring in one commit.
2. **Tests.** CEC unit: `SCREEN` present iff flag on; `screen_off` → link `Standby` + stays connected; `screen_on` → wakes on the existing link (`Active`); flag-off → `screen_*` return `NotSupported` and `SCREEN` absent. Optional daemon-level: mirror the `reachable_in_standby` mock precedent with a parallel mock flag so a `ScreenOff` *profile action* is shown to drive a CEC-style mock to standby (else rely on CEC-unit coverage + the existing screen-action dispatch tests).
3. **Docs.** [docs/cec/protocol.md](../cec/protocol.md) capability section (POWER | INPUT, plus emulated SCREEN when on); the schema helper string; CLAUDE.md CEC backend line; a one-line pointer from the vendor guide's "emulate a capability" section to this as a second worked example.

## Open questions

- **Schema field type / visibility.** A boolean toggle is not one of the current `PairingFieldType`s (which are text/path/password-shaped). Either add a `Bool`/`Toggle` field type (small core addition, reusable) or expose it as a `Diagnostic` non-editable default and let power users edit the TOML / `UpdateConfig` directly for v1. Lean toward the `Bool` field type since the GUI edit-mode should be able to flip it — but it can land as a fast follow if it bloats slice 1.

## Migration on merge

Fold the capability-model paragraph into [docs/cec/protocol.md](../cec/protocol.md) (§ capability mapping) and the "emulate a capability" cross-reference into the vendor guide; delete this plan. Per [docs/plans/README.md](README.md).
