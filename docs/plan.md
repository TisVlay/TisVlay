# Refactor: D-Bus API conformance

## Motivation

The current D-Bus API diverges from the
[D-Bus API Design Guidelines](https://dbus.freedesktop.org/doc/dbus-api-design.html)
and doesn't leverage modern zbus patterns. Conforming to best practices will:
- Enable standard tooling (`busctl get-property`, `busctl monitor`)
- Simplify the tray and future GUI (subscribe to `PropertiesChanged` instead
  of parsing custom JSON signals)
- Remove the custom `state_change_signal_emitter` task
- Fix the `Requesting name before setting up the object server` warning
- Eliminate dead code (`ConnectionChanged` signal defined but never emitted)

## Key architectural shift

**Move TV state INTO the D-Bus interface struct** instead of fetching from
`TvManager` on every property read.

Currently `TvService` is stateless — every property getter calls into
`TvManager`, which acquires a per-TV mutex and queries the backend. This
means property reads are expensive (lock + potential I/O) and change
notifications must be routed through a custom signal task.

After refactoring:
- `TvService` holds cached state (volume, muted, backlight, power, etc.)
- Backend subscription updates write into `TvService` via
  `InterfaceRef::get_mut()` and call the auto-generated
  `property_changed()` methods
- Property reads are instant (no lock, no backend call)
- `PropertiesChanged` emits automatically via zbus machinery
- The `state_change_signal_emitter` task is eliminated

This aligns with the zbus book's recommended pattern for external state
changes (book/src/service.md lines 347-384).

## Current deviations

### 1. Custom setter methods instead of writable properties

**Current:** `SetVolume(level)`, `SetMute(mute)`, `SetBacklight(level)` as
explicit methods. `SetTvOption(tv_id, key, value)` as a generic key-value
setter for config fields.

**Guideline:** "The `Set` method can be used to manipulate properties on an
object, eliminating redundant `SetPropertyName` methods."

**Fix:** Make `Volume`, `Muted`, `Backlight` writable properties with
property setters. The setter calls the backend command and returns
`zbus::fdo::Result<()>` for validation errors. zbus auto-emits
`PropertiesChanged` after a successful set.

### 2. Custom signals instead of PropertiesChanged

**Current:** Custom `StateChanged(state: s)` signal carrying a JSON string.
`ConnectionChanged(connected: b)` signal defined but never emitted (dead
code). No `PropertiesChanged` signals emitted anywhere.

**Guideline:** "Use `org.freedesktop.DBus.Properties.PropertiesChanged`
instead of individual property-change signals. This allows multiple
properties to be notified in a single signal emission."

**Fix:** Remove `StateChanged` and `ConnectionChanged` custom signals.
Use zbus auto-generated `property_changed()` methods when state updates
arrive from the backend. For batched updates (e.g., subscription push
with volume + mute), use `Properties::properties_changed()` directly.

### 3. JSON strings instead of D-Bus types

**Current:**
- `GetState() → s` — returns JSON blob
- `GetEffectiveUserProfile(tv_id) → s` — JSON blob
- `ListSystemProfiles() / ListUserProfiles() → a(ss)` — (name, json)
- `StateChanged(state: s)` — JSON blob

**Guideline:** "Sending structured strings over D-Bus should be avoided."

**Fix:** Replace `GetState()` with typed properties (clients use
`Properties.GetAll`). Replace profile JSON with `a{sv}` dicts.

### 4. Connection::Builder not used

**Current:** `connection.request_name()` called after manual interface
registration, causing the `Requesting name before setting up the object
server` warning.

**Fix:** Use `connection::Builder` pattern:
```rust
let connection = connection::Builder::session()?
    .name("org.tisvlay.Daemon")?
    .serve_at("/org/tisvlay/Manager", manager_service)?
    .serve_at("/org/tisvlay/Tv/office", tv_service)?
    .build()
    .await?;
```

### 5. ScreenOnProp naming conflict

**Current:** Property named `screen_on_prop` because `screen_on()` method
conflicts with the property name.

**Fix:** Rename property to `IsScreenOn` (standard boolean property naming).
Or make `ScreenOn` a writable boolean property — setting `true` unblanks,
setting `false` blanks.

### 6. Boolean for managed flag

**Guideline:** "Enumerated values should be used instead of booleans."

**Assessment:** Low priority. `managed` is genuinely boolean. Revisit if
a third state is ever needed.

---

## zbus patterns to use

### Auto-emitted PropertiesChanged on property set

```rust
#[zbus(property)]
fn volume(&self) -> u8 {
    self.volume  // instant read from cached state
}

#[zbus(property)]
fn set_volume(&mut self, val: u8) -> zbus::fdo::Result<()> {
    if val > 100 {
        return Err(zbus::fdo::Error::InvalidArgs("Volume must be 0-100".into()));
    }
    // Call backend to actually change the TV's volume
    // (need async — see considerations below)
    self.volume = val;
    Ok(())
    // zbus AUTOMATICALLY emits PropertiesChanged after this returns Ok
}
```

### Manual property change notification (for push updates)

When the TV's backend subscription pushes a volume change:

```rust
let iface_ref = object_server
    .interface::<_, TvService>(tv_path)
    .await?;
let mut iface = iface_ref.get_mut().await;
iface.volume = new_volume;
iface.volume_changed(iface_ref.signal_emitter()).await?;
```

### Batched PropertiesChanged

When a subscription push contains multiple changes:

```rust
let mut changed = HashMap::new();
changed.insert("Volume", Value::from(50u8));
changed.insert("Muted", Value::from(false));
Properties::properties_changed(
    emitter, interface_name, changed, Cow::Borrowed(&[]),
).await?;
```

### emits_changed_signal annotation values

| Value | Behavior | Use for |
|-------|----------|---------|
| `"true"` (default) | Emits with new value | Most properties |
| `"invalidates"` | Emits without value (client re-fetches) | Large properties (input list) |
| `"const"` | Never emits (value never changes) | Id, Vendor |
| `"false"` | Never emits | Internal-only properties |

---

## Implementation plan

### Phase A: Restructure TvService to hold state

The foundational change — everything else builds on this.

**A.1** Add state fields to `TvService`:

```rust
struct TvService {
    // Identity (const — set once at registration)
    tv_id: String,
    manager: Arc<TvManager>,

    // Cached state (updated by subscription pushes)
    connected: bool,
    power_state: String,      // "active", "standby", "screen_off"
    is_screen_on: bool,
    volume: u8,
    muted: bool,
    backlight: u8,
    current_input: String,
    inputs: Vec<(String, String, bool)>,  // (id, label, connected)

    // Config (updated by SetTvOption / config reload)
    name: String,
    managed: bool,
    system_profile: String,
    capabilities: Vec<String>,
}
```

**A.2** Initialize state at registration time.

When creating `TvService` for a TV, populate from `TvManager`'s current
state. For TVs that aren't connected yet, use defaults.

**A.3** Property getters read from cached fields (no lock, no I/O):

```rust
#[zbus(property(emits_changed_signal = "const"))]
fn id(&self) -> &str { &self.tv_id }

#[zbus(property(emits_changed_signal = "const"))]
fn vendor(&self) -> &str { /* from config */ }

#[zbus(property)]
fn volume(&self) -> u8 { self.volume }

#[zbus(property)]
fn connected(&self) -> bool { self.connected }
```

**A.4** Replace `state_change_signal_emitter` task.

Instead of routing `StateChangeEvent` through a channel → custom signal,
the subscription forwarding task (from `try_subscribe_state`) writes
directly into `TvService` via `InterfaceRef::get_mut()` and calls
`property_changed()`. This requires the forwarding task to have access
to the `ObjectServer` (or the `Connection`).

### Phase B: Writable properties

**B.1** Add property setters for controllable state:

```rust
#[zbus(property)]
async fn set_volume(&mut self, val: u8) -> zbus::fdo::Result<()> {
    self.manager.set_volume(&self.tv_id, val).await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    self.volume = val;
    Ok(())
}
```

Note: zbus property setters can be `async` — they run with `&mut self`
under the interface lock. The lock duration includes the backend I/O,
which blocks other property reads on the same interface. This is
acceptable for TV commands (fast WebSocket round-trip) but should be
noted.

**B.2** Make these writable: `Volume`, `Muted`, `Backlight`, `Managed`,
`SystemProfile`, `Name`.

**B.3** `IsScreenOn` as writable bool (set `true` = unblank, `false` = blank).

**B.4** Remove explicit command methods that are now redundant:
- `SetVolume` → property setter
- `SetMute` → property setter
- `SetBacklight` → property setter
- `ScreenOn` / `ScreenOff` → `IsScreenOn` property setter
- `SetTvOption` → individual property setters

Keep `PowerOn`, `PowerOff`, `Reconnect` as methods (they have complex
side effects beyond property changes — WoL, connection lifecycle).

### Phase C: Remove custom signals and JSON

**C.1** Remove `StateChanged` signal from Tv1.
**C.2** Remove `ConnectionChanged` signal from Tv1 (dead code).
**C.3** Remove `StateChangeEvent` enum.
**C.4** Remove `state_change_signal_emitter` task from main.rs.
**C.5** Remove `GetState()` method — replaced by `Properties.GetAll`.

**C.6** Replace JSON profile methods with typed `a{sv}` dicts:
- `ListSystemProfiles() → a{sa{sv}}`
- `ListUserProfiles() → a{sa{sv}}`
- `GetEffectiveUserProfile(tv_id) → a{sv}`
- `GetUserProfileDefault() → a{sv}`

### Phase D: Connection::Builder

**D.1** Refactor `main.rs` to use `connection::Builder`:
```rust
let connection = connection::Builder::session()?
    .name("org.tisvlay.Daemon")?
    .serve_at("/org/tisvlay", object_manager)?
    .serve_at("/org/tisvlay/Manager", manager_service)?
    .build()
    .await?;
```

**D.2** Per-TV interfaces registered dynamically via
`connection.object_server().at(path, tv_service)` after connection
is built (TVs are loaded from config at startup and can be added
at runtime via pairing).

### Phase E: Update clients

**E.1** `DaemonClient`: replace `subscribe_state_changes()` (custom
signal parsing) with `PropertiesChanged` subscription via zbus
`PropertiesProxy::receive_properties_changed()`.

**E.2** Tray: subscribe to `PropertiesChanged` on Tv1 objects.
On property change, update `CachedTvState` fields directly from
the changed properties dict — no full `fetch_all_state()` needed.
(Can keep full refresh as initial simple implementation.)

**E.3** CLI: use `Properties.Get` / `Properties.GetAll` instead of
`GetState()`.

**E.4** Session service: use `Properties.Set` for writable properties
if it calls `SetVolume` etc. (currently it calls `screen_on/off` and
`power_on/off` — `PowerOn`/`PowerOff` stay as methods).

### Phase F: Cleanup

- Remove `GetState` method and JSON serialization
- Remove `StateChangeEvent`, `emit_state_change`, command_completed/
  option_change constructors
- Remove `state_change_signal_emitter` function
- Remove `add_signal_match` / custom signal subscription code from
  `DaemonClient` (replaced by PropertiesChanged proxy)
- Update docs, busctl examples, design.md, CLAUDE.md

---

## Considerations

### Async property setters and interface locking

zbus property setters with `&mut self` hold the interface lock for the
duration of the call. If a setter does async backend I/O (WebSocket
round-trip to TV, ~20ms), all other property reads/writes on that
interface are blocked. For the tray's scroll coalescing (one set_volume
per coalesced burst), this is fine. For rapid programmatic sequences,
it could be a bottleneck.

Mitigation: the backend commands are fast (sub-millisecond daemon-side,
~20ms including TV WebSocket). The per-TV interface lock only affects
that one TV, not others.

### Subscription push → property update path

The current path: backend subscription → `TvState` on channel →
`state_change_signal_emitter` → `StateChanged` D-Bus signal.

New path: backend subscription → `TvState` on channel → task calls
`object_server.interface::<_, TvService>(path)` → `get_mut()` →
update fields → call `property_changed()`.

The forwarding task needs access to the `Connection` (for
`object_server()`). Pass it a `Connection` clone alongside the
`state_change_tx` channel, or restructure the forwarding to happen
inside `TvManager`.

### ObjectManager interaction

`Properties.GetAll` and `PropertiesChanged` work naturally with
`ObjectManager.GetManagedObjects` — the initial bulk fetch returns
all properties, and subsequent changes arrive as signals. The
`list_tvs()` client method can use `GetManagedObjects` to get all
TV state in one call (it already does, but currently only reads
identity properties, not state).

### Backward compatibility

None needed. CLI, tray, session, and daemon are versioned together.

---

## Checklist

- [ ] A.1: Add state fields to TvService
- [ ] A.2: Initialize state at registration
- [ ] A.3: Property getters read from cached fields
- [ ] A.4: Replace state_change_signal_emitter with InterfaceRef updates
- [ ] B.1: Add async property setters
- [ ] B.2: Make Volume, Muted, Backlight, Managed, SystemProfile, Name writable
- [ ] B.3: IsScreenOn as writable bool
- [ ] B.4: Remove redundant command methods
- [ ] C.1: Remove StateChanged signal
- [ ] C.2: Remove ConnectionChanged signal (dead code)
- [ ] C.3: Remove StateChangeEvent enum
- [ ] C.4: Remove state_change_signal_emitter task
- [ ] C.5: Remove GetState() method
- [ ] C.6: Typed profile data (a{sv}) instead of JSON
- [ ] D.1: connection::Builder for service registration
- [ ] D.2: Dynamic per-TV interface registration
- [ ] E.1: DaemonClient uses PropertiesChanged
- [ ] E.2: Tray subscribes to PropertiesChanged
- [ ] E.3: CLI uses Properties.Get/GetAll
- [ ] E.4: Session service updated
- [ ] F: Cleanup dead code and update docs
