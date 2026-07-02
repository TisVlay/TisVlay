# CEC screen-via-power emulation

A per-device opt-out CEC config that makes screen-blank profile actions (`screen_off` / `screen_on`) power the TV off/on, so the **shipped default profiles behave consistently across a mixed fleet** without the user rewriting them to power actions for a CEC-only setup. HDMI-CEC has no discrete screen-off (the panel can't blank while the TV stays "on"), so a device with `POWER` but no `SCREEN` otherwise ignores the stock screen events entirely.

> Status: **built** on `feat/cec-backend` (PR [#94](https://github.com/ruffsl/TisVlay/pull/94)), building on the reachable-in-standby decouple + wake-in-place already landed. Green: build · clippy `-D warnings` · 603 tests.

## Problem

The stock profiles map presence events to *screen* actions, not power: the system default's `on_dpms_off` / `on_idle_timeout` and the user default's `on_session_lock` / `on_idle_timeout` are all `ScreenOff` (mirrors for the resume side are `ScreenOn`) — see [`config.rs`](../../crates/tisvlay-core/src/config.rs). On an LG TV those blank the OLED. CEC declares `POWER | INPUT` and no `SCREEN`, so those actions hit `NotSupported` and no-op — a CEC-only user must hand-edit every profile to power actions (as in the dev `cec` session profile) to get any idle/lock behavior, and a mixed LG+CEC fleet behaves inconsistently under one shared profile.

## Decision

Add a **CEC vendor-config bool `emulate_screen_via_power` (default `true`)**, dispatched at the **daemon**, not the connection:

- A config-aware `TvConnector::emulate_screen_via_power()` predicate reports it (mirrors `reachable_in_standby` / `supports_wake` — vendor-blind: the daemon asks the backend, it doesn't know "CEC").
- The daemon's `get_capabilities` advertises `SCREEN` when the predicate is true (config-driven, so the bit is stable across connect/disconnect — no cap flicker).
- `TvManager::screen_off` / `screen_on` delegate to `power_off` / `power_on` when the predicate is true, reusing the correct reachable-in-standby power path (keep the connection, synthesize the standby cache, wake in place).

`CecConnection` stays a pure `POWER | INPUT` backend — it does **not** implement screen. This is the [vendor guide](../backends/implementing-a-vendor.md)'s "Emulating a capability the device lacks" pattern applied to `SCREEN`, with the twist that the emulation lives at the layer that owns the primitive it maps *to* (power).

### Why these choices

- **Config, default-on** — matches the project's "events map to configurable actions, never hardcoded" principle. Default-on makes a screen-blank event make *every* device go dark using its available primitive (LG blanks the panel, CEC powers off), so the stock profiles are uniform across a fleet. Opt-out (`false`) preserves the sub-stock-fidelity path for a user who'd rather not power-cycle a slow-to-wake panel on every idle blank — the CEC device then simply ignores screen actions.
- **Daemon dispatch, not connection-level** — the original plan put `screen_off`→`self.power_off()` *inside* `CecConnection`. That bypasses `TvManager::power_off`, which is the only thing that **synthesizes** the standby cache update for a *commanded* standby (a CEC TV doesn't re-broadcast a `<Standby>` it was told to do, so the monitor never pushes it). The connection-level route left the cached power state stale-`Active` after a screen-off — which would make a later `power_on` wrongly hit its idempotent no-op and *not* wake the TV. Dispatching at the daemon reuses the correct power path and avoids that whole class of bug; it also lets `SCREEN` be added config-awarely (stable caps) instead of connection-state-driven.
- **Name** — `emulate_screen_via_power` reuses the guide's "emulate a capability" vocabulary. (Alternatives weighed: `screen_falls_back_to_power`, `screen_maps_to_power`.)

### Accepted side effect

Advertising `SCREEN` also makes the GUI/tray render a **Screen on/off toggle** for the CEC TV that power-cycles. Accepted as consistent (the config says "screen means power here") rather than hiding it and routing only profile actions — a hidden toggle whose action still fires elsewhere is more surprising.

## Composition with what's landed

- `screen_off` → daemon `power_off`: standby, connection kept (reachable-in-standby decouple), standby cache synthesized. The CLAUDE.md double-dispatch note stays safe — `power_off`-to-standby is idempotent.
- `screen_on` → daemon `power_on` → wake-in-place: no teardown, no `Connected` flap, and the idempotent fast path makes a repeat `screen_on` on an already-`Active` TV a no-op.

## As built

All three slices shipped in one commit (`69383f3`):

1. **Config + dispatch.** `CecVendorConfig.emulate_screen_via_power: bool` (serde default `true`) + a `PairingFieldType::Bool` schema field (`UserInput`, default `"true"` — the `Bool` type already existed, so no core addition). `TvConnector::emulate_screen_via_power()` trait method (default `false`) + CEC override; `BackendRegistry::emulate_screen_via_power(tv_config)` predicate; daemon `get_capabilities` `| SCREEN` and `screen_off`/`screen_on` → `power_off`/`power_on` when the predicate holds. `CecConnection` and `CecBackend` static caps unchanged.
2. **Tests.** Connector predicate (default-on + opt-out); config default round-trip; and a daemon end-to-end (`screen_off` action → standby with the connection kept *and the cache accurate*, `SCREEN` advertised, `screen_on` → back on) — the cache assertion is what a connection-level route would fail. Mock parity added to drive it: `MockVendorConfig::emulate_screen_via_power` + connector override + `mock_tv_config_screen_via_power` / `with_one_screen_via_power_mock_tv` harness builders.
3. **Docs.** [docs/cec/protocol.md](../cec/protocol.md) § Screen-via-power emulation; the schema helper copy; CLAUDE.md CEC lines; and the vendor guide's "Emulating a capability the device lacks" section extended with this as a second worked example teaching *where* to emulate.

### GUI follow-ups (surfaced by hardware QA of the emulated screen)

Advertising `SCREEN` on a sub-`STOCK` CEC TV exposed three GUI issues, each fixed as its own commit:

- **View-mode config row missing** (`fix(core)`). `display_config` read only the *stored* vendor config, so a field left at its serde default (`emulate_screen_via_power`, absent from a TV paired before the field existed) showed only in edit mode. Fixed in the shared `display_rows_for_visibility`: fall back to the schema `default` when a field is absent, so view and edit agree on the effective value. General fix, not CEC-specific.
- **Leftover whitespace from capability-hidden controls** (`fix(gui)`). The PICTURE/AUDIO/INPUT groups + rows gated on capability with `visible:`, which in Slint hides rendering but keeps the layout slot and spacing — so a CEC TV (no volume/mute/backlight) left empty rows and an empty group's gap. Switched the gates to `if` conditionals so hidden elements leave the layout entirely; the flags are static per vendor, so no flicker.
- **Screen help copy was LG-specific** (`fix(gui)`). "Blank screen without powering off (instant resume)" is false for the emulated case. Reworded *statically* to cover both — a dedicated screen-blank with instant resume where supported, otherwise emulated via power — rather than swapping per backend (a user reads it once).

Deferred (noted, not built): the manual Screen toggle on a power-emulated TV duplicates the Power button. Hiding it for emulated backends (keeping the capability for profile actions) needs a small daemon→GUI "emulated" signal; left for later since the reworded help now explains the behavior.

## Migration on merge

Durable content already lives in [docs/cec/protocol.md](../cec/protocol.md) § Screen-via-power emulation and the vendor guide; delete this plan with the CEC PR. Per [docs/plans/README.md](README.md).
