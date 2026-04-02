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

## Open design questions

### Naming

The config field `hdmi_input` should become something generic. The
automation gate `only_when_active` should have a name that makes its
relationship to the input field obvious. Ideas:

| Input field | Gate option | Relationship clarity |
|-------------|------------|---------------------|
| `pc_input` | `only_when_active` | Low — "active" is vague |
| `host_input` | `only_when_host_active` | Medium — "host" is server jargon |
| `desktop_input` | `only_when_desktop_active` | Medium — what about laptops? |
| `assigned_input` | `only_when_assigned` | High — gate fires when on the assigned input |
| `bound_input` | `only_when_bound` | Medium — "bound" is technical |

The gate option should be an enum, not a boolean, to allow future
behaviors beyond simple on/off. Possible enum values:

```rust
enum InputGate {
    /// Always apply automation regardless of current input.
    Always,
    /// Only apply automation when the TV is on the assigned input.
    WhenAssigned,
    /// Skip automation when the TV is on the assigned input (inverse).
    WhenNotAssigned,
}
```

This lives in the system profile since it gates system-level automation
(startup, shutdown, sleep/wake, no_session), similar to how `managed`
controls automation participation. Could also apply to user profile
events (session lock/unlock, idle) — TBD.

**Decision needed:** final names for the input field and gate enum.

### Input ID format mismatch (existing bug)

The LG backend has two different ID systems for inputs:

- **`getExternalInputList`** returns physical IDs: `"HDMI_1"`, `"HDMI_2"`, etc.
- **`getForegroundAppInfo`** returns app IDs: `"com.webos.app.hdmi1"`, `"com.webos.app.livetv"`, etc.

The tray's input submenu shows `inputs` (physical IDs with labels) as
radio options but compares against `current_input` (app ID) to select
the current one — **these never match**, so the current input is never
highlighted in the tray menu.

The `pc_input` / `assigned_input` config field would store one of these
formats. The automation gate needs to compare it against the TV's
current input. Which format should be canonical?

Options:
- **Physical ID** (`"HDMI_1"`): matches what the user sees in the tray
  and in the TV's own input picker. Requires mapping app IDs → physical
  IDs for comparison (e.g., `"com.webos.app.hdmi1"` → `"HDMI_1"`).
- **App ID** (`"com.webos.app.hdmi1"`): matches what the TV pushes via
  subscriptions. But non-HDMI "inputs" like "Sling TV" don't have
  physical IDs — they're only app IDs.
- **Normalized**: store the physical ID, but the backend provides a
  mapping table. The daemon compares using the mapping.

The backend should own the mapping since it's vendor-specific. The
`InputInfo` struct already has both `id` (physical) and could gain
an `app_id` field. Or the backend normalizes `current_input` from
app ID to physical ID before exposing it.

**Decision needed:** canonical input ID format and where mapping lives.

### Scope of the input gate

Where should the input gate apply?

| Event type | Owner | Gate applies? | Notes |
|-----------|-------|:---:|-------|
| on_startup | System profile | Yes | Don't WoL if user is watching a console |
| on_shutdown | System profile | Maybe | Still want to power off even if on another input? |
| on_sleep | System profile | Yes | Don't blank if watching a console |
| on_wake | System profile | Yes | Don't power on if was watching a console |
| on_reboot | System profile | Maybe | |
| on_no_session | System profile | Yes | |
| on_session_lock | User profile | Yes | Don't blank during console gaming |
| on_session_unlock | User profile | Yes | |
| on_idle_timeout | User profile | Yes | Don't blank if watching a movie on console |
| on_greeter_wake | User profile | Yes | |

If the gate is in the system profile, it only applies to system events.
If it's in both profile types, it applies to everything. If it's on
`TvConfig`, it applies to all events globally for that TV.

**Decision needed:** gate scope (system profile, both profiles, or TvConfig).

---

## Implementation sketch (pending decisions above)

### Phase 1: Fix input ID mismatch

- Add `app_id: Option<String>` to `InputInfo` in tisvlay-core
- LG backend populates `app_id` from the app ID mapping
- Backend normalizes `current_input` to physical ID (or provides both)
- Fix tray input submenu comparison to work correctly

### Phase 2: Rename `hdmi_input` → `assigned_input` (or chosen name)

- Rename field in `TvConfig`
- Update all config examples, test fixtures, docs
- Update `SetTvOption` key name
- Update design.md

### Phase 3: Implement input gate

- Add `InputGate` enum (or chosen name) to `SystemProfileConfig`
- Implement gate check in `execute_system_event()` — before dispatching
  each TV's action, check if the gate allows it based on current input
- Requires the daemon to know each TV's current input — which it now
  does via push subscriptions (subscription updates include
  `getForegroundAppInfo` which pushes current app/input changes)

### Phase 4: Surface in CLI, tray, docs

- CLI: show current input and gate status in `tisvlay status`
- Tray: show current input in tooltip, fix radio selection
- Config docs: examples showing the new field and gate enum

---

## Notes

- The `managed` flag and the input gate serve similar purposes (gating
  automation) but at different levels: `managed` is a global kill switch,
  the input gate is conditional. They compose: `managed = false` skips
  everything; `managed = true` + `input_gate = WhenAssigned` + TV not
  on assigned input → skips.

- The push subscription for `getForegroundAppInfo` already updates
  `current_input` in real time. The gate check can read this without
  additional polling.

- For the GUI: the pairing wizard should show available inputs (from
  `getExternalInputList`) and let the user pick their PC's input. This
  sets `assigned_input` and optionally enables the gate.
