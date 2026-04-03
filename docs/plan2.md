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
This aligns with the D-Bus refactor (typed properties instead of JSON),
and `current_input` uses the same normalized ID space as the input list,
making matching trivial.

### Gate scope and D-Bus exposure (decided)

`automate_when` lives in `SystemProfileConfig`. The daemon computes the
gate result and exposes it as read-only D-Bus properties on Tv1:

**D-Bus properties on `org.tisvlay.Tv1`:**

| Property | Type | Access | Description |
|----------|------|--------|-------------|
| `Managed` | `b` | read-write | Global automation kill switch (existing) |
| `PrimaryInput` | `s` | read-write | Which input this PC is connected to |
| `AutomateWhen` | `s` | read-only | From resolved system profile: `"always"`, `"on_primary"`, `"off_primary"` |
| `Automated` | `b` | read-only | **Computed**: `managed && (automate_when == always \|\| input match)` |

`CurrentInput` already exists as a read-only property on
`org.tisvlay.Tv1.Input` (cached, updated by subscription pushes).

`Automated` is the single boolean that any automation client checks
before acting. It folds together `managed`, `automate_when`, and the
current input state into one answer: "should I automate this TV right
now?"

- `Managed` stays separate for the global kill switch use case
- `PrimaryInput`, `AutomateWhen` stay separate for
  diagnostic/GUI display (so the user can see *why* automation is
  disabled)
- `Automated` is the convenience property for simple callers

**Automation clients (session service, custom services) use this pattern:**

```
if tv.automated {
    daemon.set_property(tv_id, "IsScreenOn", false);  // or any action
}
```

Manual actions from the tray and CLI skip the `Automated` check — they
always execute regardless of input state.

This approach:
- Scales to arbitrary automation sequences (no structured action strings)
- Works with the D-Bus property architecture (writable properties,
  PropertiesChanged)
- Keeps policy enforcement centralized in the daemon (single source of
  truth for `Automated`)
- Lets any client (session, custom service, GUI) consume the gate without
  reimplementing the logic

**Gate composition:**

| `managed` | `automate_when` | TV on primary? | `Automated` |
|:---------:|:---------------:|:--------------:|:-----------:|
| false | any | any | **false** |
| true | always | any | **true** |
| true | on_primary | yes | **true** |
| true | on_primary | no | **false** |
| true | on_primary | unknown (TV off) | **true** (benefit of doubt) |
| true | off_primary | yes | **false** |
| true | off_primary | no | **true** |

---

## Implementation plan

### Phase 1: Fix input ID normalization and tray radio bug

**Files:** `crates/tisvlay-lg/src/backend.rs`, `crates/tisvlay-core/src/state.rs`

- LG backend: add app-ID-to-physical-ID mapping (e.g.,
  `"com.webos.app.hdmi1"` → `"HDMI_1"`)
- Normalize `current_input` in `update_state_from_subscription()` when
  handling `getForegroundAppInfo` updates — map the app ID to the
  physical ID from the input list
- Tray input submenu: `current_input` now matches `inputs[i].id` in the
  D-Bus `CurrentInput` property, so radio selection works correctly
  (tray reads these from `TvInfo` via `GetManagedObjects`)
- Test: switch inputs with the TV remote, verify tray radio updates

### Phase 2: Rename `hdmi_input` → `primary_input`

**Files:** `crates/tisvlay-core/src/config.rs`, all config TOML fixtures,
`crates/tisvlayd/src/tv_manager.rs`, docs

- Rename `TvConfig::hdmi_input` → `TvConfig::primary_input`
- Rename `SetTvOption` key from `"hdmi_input"` → `"primary_input"` in
  `apply_tv_option()`
- Remove `only_when_active` field from `TvConfig` (dead code)
- Remove `"only_when_active"` key from `apply_tv_option()`
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

### Phase 4: Implement gate in daemon and expose D-Bus properties

**Files:** `crates/tisvlayd/src/dbus/tv.rs`, `crates/tisvlayd/src/main.rs`,
`crates/tisvlayd/src/tv_manager.rs`

- Add `primary_input` and `automate_when` cached fields to `TvService`
  struct, populated from config at registration time
- Add D-Bus properties on `org.tisvlay.Tv1`:
  - `PrimaryInput` (read-write) — writable property setter delegates to
    `set_tv_option("primary_input", ...)`
  - `AutomateWhen` (read-only) — from resolved system profile
  - `Automated` (read-only, computed) — the gate result
- `Automated` getter reads `managed`, `automate_when`, `primary_input`
  from `TvService` cache and `current_input` from `TvInputService` cache
  (via `ObjectServer::interface::<_, TvInputService>(path)`)
- In `execute_system_event()`, replace the `managed`-only check with
  the `Automated` gate: query the `TvService` and `TvInputService` caches
  to evaluate the gate in-process (no D-Bus round-trip)
- Update `state_change_forwarder` `OptionChanged` handler to update
  `primary_input` cache when changed via `SetTvOption`
- When TV is off (no known current input), `Automated` returns `true`
  (benefit of doubt — can't check input in standby)

### Phase 5: Update session service to check `Automated`

**File:** `crates/tisvlay-session/src/main.rs`

- `TvInfo` already includes all properties from `GetManagedObjects`, so
  add `automated` field to `TvInfo` (read from D-Bus property)
- In the session event handler, replace the `managed`-only filter with
  `tv.automated` from `list_tvs()`
- This means session service automatically respects input-aware gating
  without any additional D-Bus calls

### Phase 6: Surface in CLI, tray, docs

- CLI `tisvlay status`: show `primary_input`, `current_input`, and
  `automated` state
- Tray tooltip: show current input name
- Tray: `Automated` status could be shown alongside `Managed` in the
  TV submenu (or as a combined indicator)
- Config examples and design.md

---

## Checklist

- [ ] Phase 1: Fix input ID normalization and tray radio bug
- [ ] Phase 2: Rename hdmi_input → primary_input, remove only_when_active
- [ ] Phase 3: Add AutomateWhen enum to SystemProfileConfig
- [ ] Phase 4: Implement gate in daemon, expose D-Bus properties
- [ ] Phase 5: Update session service to check Automated
- [ ] Phase 6: Surface in CLI, tray, docs

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
