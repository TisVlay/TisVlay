# Plan: Input-aware automation and input field refactor

## Motivation

Two related issues:

1. **`only_when_active` is dead code.** The field exists in config, is
   parseable and settable, but nothing reads it to gate automation.
   The original intent: skip automation when the TV is showing a
   different input (e.g., don't blank the screen on session lock if
   someone is using a game console).

2. **`hdmi_input` is too specific.** Not all inputs are HDMI — TVs have
   DVI, AV, coaxial, and even WebOS apps (e.g., "Sling TV") reported
   as inputs. The field needs a vendor-agnostic name.

## Design decisions

### Naming (decided)

- **`primary_input`** on `TvConfig` — the input this PC is connected to.
  Vendor-agnostic; not all inputs are HDMI (could be DVI, AV, coaxial,
  or even a WebOS app ID like "Sling TV").

- **`automate_when`** on `SystemProfileConfig` — enum controlling when
  automation fires relative to the TV's current input:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum AutomateWhen {
    /// Always apply automation regardless of current input.
    #[default]
    Always,
    /// Only apply automation when the TV is on the primary input.
    OnPrimary,
    /// Only apply automation when the TV is NOT on the primary input.
    OffPrimary,
}
```

`primary_input` is a per-TV hardware fact (which port the PC plugs into).
`automate_when` is a behavioral policy shared across TVs via profiles.
An admin can have multiple TVs with different `primary_input` values but
the same `automate_when` policy via a shared system profile.

```toml
[system.profiles.default]
automate_when = "always"

[system.profiles.shared]
automate_when = "on_primary"    # only automate when showing the PC

[[tv]]
id = "living-room"
primary_input = "HDMI_1"
profile.system = "default"

[[tv]]
id = "guest-room"
primary_input = "HDMI_2"
profile.system = "shared"       # same policy, different physical input
```

### Input ID normalization (decided)

The LG backend has two ID systems:

- **`getExternalInputList`**: physical IDs `"HDMI_1"`, `"HDMI_2"`
- **`getForegroundAppInfo`**: app IDs `"com.webos.app.hdmi1"`, `"com.webos.app.livetv"`

**Existing bug:** The tray's input submenu compares app IDs against
physical IDs — they never match, so the current input is never
highlighted and radio buttons stick when cycling through options.

**Decision:** The backend normalizes `current_input` to the physical ID
space. `InputInfo` exposes:
- `id`: normalized stable identifier (e.g., `"HDMI_1"`) — used for
  config, matching, automation, D-Bus properties
- `label`: human-friendly display name (e.g., `"HDMI 1"` or user-renamed
  `"Gaming PC"`) — used for UI rendering

The backend owns the vendor-specific app-ID-to-physical-ID mapping.
This aligns with the D-Bus refactor plan (typed `a(ssb)` properties
instead of JSON), and `current_input` uses the same normalized ID space
as the input list, making matching trivial.

### Gate scope (decided)

`automate_when` lives in `SystemProfileConfig` and gates all system
events (startup, shutdown, sleep, wake, reboot, no_session).

The gate composes with `managed`:
- `managed = false` → skips all automation (global kill switch)
- `managed = true` + `automate_when = on_primary` + TV not on primary
  → skips system event automation
- `managed = true` + `automate_when = always` → always automates

For user events (session lock/unlock, idle), a future `automate_when`
could be added to `UserProfileConfig` if needed. For now, system
profile only — user events are session-scoped and the session service
doesn't currently know the TV's current input.

---

## Implementation plan

### Phase 1: Fix input ID normalization and tray radio bug

**Files:** `crates/tisvlay-lg/src/backend.rs`, `crates/tisvlay-core/src/state.rs`,
`crates/tisvlay-tray/src/tray.rs`

- LG backend: add app-ID-to-physical-ID mapping (e.g.,
  `"com.webos.app.hdmi1"` → `"HDMI_1"`)
- Normalize `current_input` in `update_state_from_subscription()` when
  handling `getForegroundAppInfo` updates — map the app ID to the
  physical ID from the input list
- Fix tray input submenu: `current_input` now matches `inputs[i].id`,
  so radio selection works correctly
- Test: switch inputs with the TV remote, verify tray radio updates

### Phase 2: Rename `hdmi_input` → `primary_input`

**Files:** `crates/tisvlay-core/src/config.rs`, all config TOML fixtures,
`crates/tisvlayd/src/tv_manager.rs`, docs

- Rename `TvConfig::hdmi_input` → `TvConfig::primary_input`
- Rename `SetTvOption` key from `"hdmi_input"` → `"primary_input"`
- Remove `only_when_active` field from `TvConfig` (dead code)
- Update all config examples, test fixtures, design.md, CLAUDE.md
- Update `startup_power_on()` which uses `hdmi_input` for input
  switching after WoL

### Phase 3: Add `automate_when` to SystemProfileConfig

**Files:** `crates/tisvlay-core/src/config.rs`, `crates/tisvlayd/src/tv_manager.rs`

- Add `AutomateWhen` enum to config.rs
- Add `automate_when: Option<AutomateWhen>` to `SystemProfileConfig`
  (Option for merge support, default `Always`)
- Add `automate_when: AutomateWhen` to `ResolvedSystemProfile`
- Update merge/resolve logic

### Phase 4: Implement input gate in daemon event dispatch

**File:** `crates/tisvlayd/src/tv_manager.rs`

- In `execute_system_event()`, before dispatching each TV's action:
  1. Read the TV's resolved system profile → `automate_when`
  2. If `Always` → proceed
  3. If `OnPrimary` → check TV's current input against `primary_input`;
     skip if not matching
  4. If `OffPrimary` → inverse check
- The daemon already knows each TV's current input via push
  subscriptions (`getForegroundAppInfo` updates `current_input` in
  real time). Store `current_input` on `ManagedTv` alongside the
  connection.
- On startup, if `automate_when = on_primary` and the TV is off (no
  current input known), `on_startup` should still fire — can't check
  input when the TV is in standby. The gate only applies when the TV
  is already on and has a known current input.

### Phase 5: Surface in CLI, tray, D-Bus

- CLI `tisvlay status`: show `primary_input` and current input
- Tray tooltip: show current input name
- D-Bus: `PrimaryInput` property on Tv1 (read-write, aligns with
  D-Bus refactor plan)
- Config examples and design.md

---

## Checklist

- [ ] Phase 1: Fix input ID normalization and tray radio bug
- [ ] Phase 2: Rename hdmi_input → primary_input, remove only_when_active
- [ ] Phase 3: Add AutomateWhen enum to SystemProfileConfig
- [ ] Phase 4: Implement input gate in daemon event dispatch
- [ ] Phase 5: Surface in CLI, tray, D-Bus, docs

---

## Notes

- The `managed` flag and `automate_when` compose:
  `managed = false` → skips all automation (global kill switch);
  `managed = true` + `automate_when = on_primary` + TV not on primary
  → skips system event automation.

- The push subscription for `getForegroundAppInfo` already updates
  `current_input` in real time. The gate check reads this without
  additional polling.

- For the GUI: the pairing wizard should show available inputs (from
  `getExternalInputList`) and let the user pick their PC's input. This
  sets `primary_input` and the admin can assign a system profile with
  `automate_when = on_primary`.

- For user events (session lock/unlock, idle): a future `automate_when`
  could be added to `UserProfileConfig`. This requires the session
  service to know the TV's current input (query daemon or subscribe).
  Deferred — system profile coverage is sufficient for now.
