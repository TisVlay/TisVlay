# Refactor: D-Bus API conformance

## Motivation

The current D-Bus API diverges from the
[D-Bus API Design Guidelines](https://dbus.freedesktop.org/doc/dbus-api-design.html)
in several ways. Conforming to best practices will improve interoperability,
debuggability (standard tools understand standard patterns), and simplify
the GUI client implementation.

## Current deviations

### 1. Custom setter method instead of writable properties

**Current:** `SetTvOption(tv_id, key, value)` on Manager1 — a generic
key-value setter that handles `managed`, `profile.system`, `hdmi_input`,
`only_when_active`, `name`.

**Guideline:** "Use the standard `org.freedesktop.DBus.Properties` Set
method instead of custom `SetPropertyName` methods." Settable values
should be writable D-Bus properties on the per-TV object, changed via
`org.freedesktop.DBus.Properties.Set`.

**Fix:** Make `Managed`, `SystemProfile`, `Name`, `HdmiInput`,
`OnlyWhenActive` writable properties on `org.tisvlay.Tv1`. Remove
`SetTvOption` from Manager1. The property setter implementation handles
validation, re-resolution, persistence, and signal emission.

### 2. Custom state signal instead of PropertiesChanged

**Current:** Custom `StateChanged(state: s)` signal carries a JSON string
with changed state. Custom `ConnectionChanged(connected: b)` signal.
Properties like `Managed` and `SystemProfile` have
`emits_changed_signal = "false"`.

**Guideline:** "Use `org.freedesktop.DBus.Properties.PropertiesChanged`
instead of individual property-change signals. This allows multiple
properties to be notified in a single signal emission."

**Fix:** Add TV state as D-Bus properties on Tv1 (`Volume`, `Muted`,
`Backlight`, `PowerState`, `ScreenOn`, `CurrentInput`, `Inputs`).
Enable `PropertiesChanged` emission on all properties. Remove custom
`StateChanged` and `ConnectionChanged` signals. The daemon updates
these properties when the LG backend pushes subscription updates, and
zbus automatically emits `PropertiesChanged`.

### 3. JSON strings instead of D-Bus types

**Current:** Several methods pass structured data as JSON strings:
- `StateChanged(state: s)` — JSON blob
- `GetEffectiveUserProfile(tv_id) → s` — JSON blob
- `ListSystemProfiles() → a(ss)` — array of (name, json_string)
- `ListUserProfiles() → a(ss)` — array of (name, json_string)
- `GetState() → s` (via D-Bus) — returns JSON

**Guideline:** "Sending structured strings over D-Bus should be avoided,
as they need to be built and parsed; both are complex operations which
are prone to bugs."

**Fix:** Use proper D-Bus types:
- Profile lists: `a(sa{sv})` — array of (name, dict of variant values)
- Effective profile: `a{sv}` — dict of variant values
- State: individual properties (see #2)
- Remove `GetState()` method — clients read properties instead

### 4. Boolean for managed flag

**Current:** `Managed` is a boolean property.

**Guideline:** "Enumerated values should be used instead of booleans, as
they allow extra values to be added in future, and there is no ambiguity
about the sense of the boolean value."

**Assessment:** Low priority. `managed` is a clear boolean with no
foreseeable third state. Revisit only if we add states like "paused"
or "limited" automation.

### 5. GetState() as a bulk query

**Current:** `GetState()` method returns all TV state in one call.

**Guideline:** Properties with `GetAll` on the standard Properties
interface achieves the same thing with type safety and change
notifications built in.

**Fix:** Replace `GetState()` with D-Bus properties. Clients use
`org.freedesktop.DBus.Properties.GetAll("org.tisvlay.Tv1")` for bulk
reads, and `PropertiesChanged` signals for push updates.

---

## Implementation plan

### Phase A: TV state as D-Bus properties

The highest-impact change — enables `PropertiesChanged` for the tray
and GUI, eliminates JSON strings, and enables standard D-Bus tooling
(`busctl get-property`, `busctl monitor`).

**A.1** Add state properties to `org.tisvlay.Tv1`:

| Property | Type | Access | Notes |
|----------|------|--------|-------|
| `PowerState` | `s` | read | "Active", "Standby", "ScreenOff", etc. |
| `ScreenOn` | `b` | read | |
| `Volume` | `y` | read-write | 0–100; setter calls backend |
| `Muted` | `b` | read-write | setter calls backend |
| `Backlight` | `y` | read-write | 0–100; setter calls backend |
| `CurrentInput` | `s` | read | App/input ID |
| `Inputs` | `a(ssb)` | read | Array of (id, label, connected) |

Read-write properties use zbus property setters that call the backend
command (e.g., `set_volume`) and emit `PropertiesChanged` on success.

**A.2** Update daemon to push property changes:

When the LG backend subscription pushes a state update, the daemon
updates the per-TV D-Bus properties. zbus emits `PropertiesChanged`
automatically for properties that have `emits_changed_signal = "true"`.

This replaces the current `StateChangeEvent::TvStateChanged` → custom
`StateChanged` signal path.

**A.3** Update tray to subscribe to `PropertiesChanged`:

Replace the custom signal subscription with
`org.freedesktop.DBus.Properties.PropertiesChanged` on Tv1 objects.
The tray can parse the changed properties dict directly instead of
doing a full `fetch_all_state()` refresh — but the initial approach
can keep the full refresh for simplicity.

**A.4** Remove `GetState()` method and custom `StateChanged` signal.

Clients use `Properties.GetAll("org.tisvlay.Tv1")` instead.
`ConnectionChanged` may be kept as a convenience or replaced by
watching the `Connected` property change.

---

### Phase B: Writable properties replace SetTvOption

**B.1** Make `Managed`, `SystemProfile`, `Name`, `HdmiInput`,
`OnlyWhenActive` writable D-Bus properties on Tv1.

Each property setter:
1. Validates the new value
2. Updates in-memory config
3. Persists to disk (via `spawn_blocking`)
4. zbus emits `PropertiesChanged` automatically

**B.2** Remove `SetTvOption` from Manager1.

**B.3** Update CLI, tray to use `Properties.Set` instead of `SetTvOption`.

---

### Phase C: Typed profile data instead of JSON strings

**C.1** Change profile list methods to return `a{sv}` dicts:
- `ListSystemProfiles() → a{sa{sv}}`
- `ListUserProfiles() → a{sa{sv}}`
- `GetEffectiveUserProfile(tv_id) → a{sv}`
- `GetUserProfileDefault() → a{sv}`

**C.2** Update `DaemonClient` and `SessionClient` to deserialize the
typed D-Bus values instead of parsing JSON.

**C.3** Update tray and CLI to work with typed profile data.

---

### Phase D: Cleanup

- Remove `StateChangeEvent::TvStateChanged` (replaced by property changes)
- Remove `state_change_signal_emitter` task (replaced by zbus property
  emission)
- Remove JSON serialization in profile D-Bus methods
- Update docs, busctl examples, design.md

---

## Considerations

### zbus property change emission

zbus can emit `PropertiesChanged` automatically when a property value
changes, but only if the property is stored in a way that zbus can
detect changes. This may require storing TV state in the D-Bus service
struct (TvService) rather than behind per-TV mutexes in TvManager.
Need to investigate how zbus handles async property access and change
notification — see `emits_changed_signal` annotation options.

### Backward compatibility

No backward compatibility constraints. The CLI, tray, session service,
and daemon are all versioned together. The D-Bus introspection XML will
change but no external consumers exist.

### Signal volume with PropertiesChanged

Each property change emits a separate `PropertiesChanged` signal unless
batched. Rapid volume changes from the TV remote would produce many
signals. The tray's 100ms debounce handles this, but the daemon should
batch property updates from a single subscription push into one
`PropertiesChanged` emission if possible.

---

## Checklist

- [ ] A.1: Add TV state D-Bus properties to Tv1
- [ ] A.2: Push backend subscription updates to D-Bus properties
- [ ] A.3: Tray subscribes to PropertiesChanged
- [ ] A.4: Remove GetState() and custom StateChanged signal
- [ ] B.1: Make Managed, SystemProfile, etc. writable properties
- [ ] B.2: Remove SetTvOption
- [ ] B.3: Update CLI and tray for Properties.Set
- [ ] C.1: Typed profile data in D-Bus methods
- [ ] C.2: Update DaemonClient/SessionClient
- [ ] C.3: Update tray and CLI
- [ ] D: Cleanup dead code and update docs
