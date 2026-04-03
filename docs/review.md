# Review: D-Bus conformance refactor + Input-aware automation (e3f0a78..b9faa7f)

Review of 21 commits spanning two plans:
1. **plan.md** (Phases A–F): D-Bus API conformance refactor — cached state in interface
   structs, writable properties, standard PropertiesChanged, connection::Builder
2. **plan2.md** (Phases 1–6): Input-aware automation — primary_input, AutomateWhen enum,
   Automated D-Bus property, LG input ID normalization

Build: clean. Tests: all pass.

---

## T1. Phase A: Cached state in D-Bus interface structs — excellent architecture

**File:** [crates/tisvlayd/src/dbus/tv.rs](crates/tisvlayd/src/dbus/tv.rs)

Six service structs now hold cached state with correct `emits_changed_signal` annotations:
- `const` for identity props (`Id`, `Vendor`, `Address`) — never change
- `true` (default) for most state props — auto-emit new value
- `invalidates` for `Capabilities` and `AvailableInputs` — large/complex, client re-fetches

Property reads are now instant (no lock, no backend I/O). The forwarder task in `main.rs`
updates cached fields via `InterfaceRef::get_mut()` with change detection — only emits
`property_changed()` when the value actually differs.

This eliminates the entire `state_change_signal_emitter` task and custom JSON signals.
The D-Bus property cache is now the single source of truth for TV state.

**Severity:** Info (correct architecture, major improvement)

---

## T2. Phase B: Writable properties — clean replacement of setter methods

Volume, Muted, Backlight, IsScreenOn, Name, Managed, SystemProfile, and PrimaryInput
are all writable properties. The setters call the backend (or `set_tv_option`) and update
the cache on success, letting zbus auto-emit PropertiesChanged.

The `IsScreenOn` rename (from `ScreenOnProp`) is a good fix — `set_is_screen_on(true)`
reads naturally as "unblank", `set_is_screen_on(false)` as "blank".

Volume and backlight setters validate range (0-100) and return `zbus::fdo::Error::InvalidArgs`.

`PowerOn`, `PowerOff`, `Reconnect`, `SwitchInput` correctly remain as methods — they
have complex lifecycle side effects beyond property changes.

**Severity:** Info (correct API design)

---

## T3. Phase C: Custom signals removed, PropertiesChanged everywhere

`StateChanged` and `ConnectionChanged` custom signals are removed. All state updates
flow through standard `PropertiesChanged`. The client's `subscribe_state_changes()`
now matches `org.freedesktop.DBus.Properties.PropertiesChanged` on TV paths instead
of custom signal interfaces.

`TvStateChange` simplified from `StateChanged { json }` / `ConnectionChanged { bool }`
to `PropertiesChanged { tv_id }` — the subscriber triggers a full refresh, which is
correct since `GetManagedObjects` now returns all state in one call.

`StateChangeEvent` in tv_manager cleaned up: `TvStateChanged { json }` replaced with
typed `SubscriptionUpdate { TvState }`, `ConnectionChanged { bool }`, and
`OptionChanged { key, value }`. No more JSON serialization on the internal path.

**Severity:** Info (clean removal of custom signals)

---

## T4. Phase D: connection::Builder — fixes startup warning

**File:** [crates/tisvlayd/src/main.rs](crates/tisvlayd/src/main.rs)

ObjectManager and Manager1 registered via `builder.serve_at()` before `.build()`,
so they're available before the well-known name is claimed. Per-TV interfaces remain
dynamically registered after build (correct — TVs are loaded from config at startup
and added at runtime via pairing).

`ManagerService` no longer stores a `Connection` field — `add_tv` and `remove_tv`
receive it via `#[zbus(connection)]` parameter injection. Clean use of modern zbus.

**Severity:** Info (correct pattern)

---

## T5. Phase E+F: GetState removed, TvInfo extended with cached state

**File:** [crates/tisvlay-core/src/dbus_client.rs](crates/tisvlay-core/src/dbus_client.rs)

`TvInfo` now includes `power_state`, `is_screen_on`, `volume`, `muted`, `current_input`,
`backlight`, `inputs` — all extracted from sub-interface properties in the
`GetManagedObjects` response. A single bulk call replaces N per-TV `GetState` round-trips.

The tray's `fetch_all_state` no longer calls `get_state()` per TV with timeout handling.
It reads everything from `TvInfo` directly. This eliminates the 2-10s timeout per
disconnected TV that made the tray feel sluggish when TVs were powered off.

8 dead `TvManager` getter methods removed (get_power_state, is_screen_on, get_volume,
is_muted, get_current_input, get_inputs, get_backlight, get_state).

**Severity:** Info (significant UX and code improvement)

---

## T6. `AutomateWhen::should_automate()` — correct gate logic

**File:** [crates/tisvlay-core/src/config.rs](crates/tisvlay-core/src/config.rs)

```rust
pub fn should_automate(self, primary_input: Option<&str>, current_input: Option<&str>) -> bool {
    match self {
        Self::Always => true,
        Self::OnPrimary => match (primary_input, current_input) {
            (None, _) | (_, None) => true,   // benefit of the doubt
            (Some(primary), Some(current)) => current == primary,
        },
        ...
    }
}
```

Benefit-of-the-doubt semantics when input is unknown (TV off, no subscription data):
returns `true` so automation fires for boot wake scenarios where the TV hasn't reported
its input yet. This matches the truth table in plan2.md.

The `Display` impl for `AutomateWhen` returns the serde name (e.g., `"on_primary"`),
used by the D-Bus `AutomateWhen` property.

**Severity:** Info (correct semantics)

---

## T7. `ManagedTv::is_automated()` — daemon-side gate evaluation

**File:** [crates/tisvlayd/src/tv_manager.rs](crates/tisvlayd/src/tv_manager.rs)

```rust
fn is_automated(&self, managed: bool) -> bool {
    if !managed { return false; }
    self.system_profile.automate_when.should_automate(
        self.config.primary_input.as_deref(),
        self.last_known_input.as_deref(),
    )
}
```

`last_known_input` is updated by the subscription forwarding task when
`state.current_input` changes. The `execute_system_event` method now calls
`list_automated_tv_ids()` instead of `list_managed_tv_ids()`.

Note: `list_automated_tv_ids()` acquires per-TV locks (for `is_automated`), unlike
`list_managed_tv_ids()` which only read the outer flag. This reintroduces the lock
contention concern from H12, but the `is_automated` call is pure computation (no I/O),
so the per-TV lock is held for nanoseconds. Acceptable.

**Severity:** Info (correct, lock concern is negligible)

---

## T8. LG input ID normalization — fixes tray radio bug

**File:** [crates/tisvlay-lg/src/backend.rs](crates/tisvlay-lg/src/backend.rs)

`TvState` gains `input_app_id_map: HashMap<String, String>` built from
`getExternalInputList` (maps app ID like `com.webos.app.hdmi1` to physical ID
`HDMI_1`). The `getForegroundAppInfo` handler normalizes `current_input` through
this map.

The map is cleared and rebuilt on each `getExternalInputList` update (correct —
inputs can be hotplugged). Non-input apps (Netflix, etc.) fall back to the raw
app ID, which is the right behavior (they won't match any `primary_input`).

**Severity:** Info (correct fix for long-standing bug)

---

## T9. `f191e75`: Initial subscription state sent immediately

**File:** [crates/tisvlay-lg/src/backend.rs:264-268](crates/tisvlay-lg/src/backend.rs#L264-L268)

```rust
let _ = state_tx.send(initial_state.clone());
```

After seeding the cached state from subscribe responses, the initial state is
pushed to the channel immediately. This ensures D-Bus properties are populated
before any client reads them — not just after the first subsequent WebSocket push.

Without this, there was a window between connection and first TV remote interaction
where all cached properties were at defaults.

**Severity:** Info (correct fix)

---

## T10. Dual logging — both stderr and journald from TTY

**File:** [crates/tisvlay-core/src/logging.rs](crates/tisvlay-core/src/logging.rs)

When running interactively (`is_terminal(stderr)`), logs now go to both stderr
(with timestamps, for the terminal) and journald (if available, for `journalctl`
queries). This is implemented with a layered subscriber.

Previously, interactive mode only logged to stderr. Developers had to choose
between terminal output and structured journal queries. Now they get both.

**Severity:** Info (good DX improvement)

---

## T11. Session service gates on `Automated` instead of `Managed`

**File:** [crates/tisvlay-session/src/main.rs](crates/tisvlay-session/src/main.rs)

The event handler's filter changed from `t.managed` to `t.automated` on the
`TvInfo` from `list_tvs()`. This means a TV with `automate_when = on_primary`
that's currently showing a game console input will be skipped for session events
(screen lock/unlock, idle) — exactly the intended behavior.

The `TvInfo.automated` field is populated from the D-Bus `Automated` property
in the `GetManagedObjects` response, so the session service doesn't need to
evaluate the gate itself.

**Severity:** Info (correct integration)

---

## T12. Tray icon reflects automation gate state

**File:** [crates/tisvlay-tray/src/tray.rs](crates/tisvlay-tray/src/tray.rs)

Icon selection changed from `(connected, managed)` to `(connected, automated)`:
- `tvconnected` (green) when automation is active
- `tvtrusted` (neutral) when automation is paused by input-aware gating
- `tvdisconnected` when disconnected

The user can see at a glance whether automation is currently active for each TV,
reflecting the composite gate state (managed + automate_when + input).

**Severity:** Info (good UX)

---

## T13. `state.rs` `input_app_id_map` is an internal detail on `TvState`

**File:** [crates/tisvlay-core/src/state.rs](crates/tisvlay-core/src/state.rs)

`TvState` gains a `pub input_app_id_map: HashMap<String, String>` field. This is
an LG-specific internal mapping that leaks into the vendor-agnostic `TvState` type.
Other backends (Samsung, Sony) wouldn't need this field.

However, `TvState` already contains `current_app: Option<String>` which is also
LG-specific. The pragmatic approach is acceptable — `TvState` is a union of fields
that different backends may or may not populate, with `Option` and empty defaults
for unused fields. The `input_app_id_map` follows this pattern.

If vendor-specific state ever becomes unwieldy, it could move behind a
`vendor_state: Box<dyn Any>` or a separate struct. Not needed now.

**Severity:** Low (minor abstraction leak, consistent with existing pattern)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| T1 | **Info** | Cached state in D-Bus structs — correct architecture |
| T2 | **Info** | Writable properties replace setter methods |
| T3 | **Info** | PropertiesChanged replaces custom signals |
| T4 | **Info** | connection::Builder fixes startup warning |
| T5 | **Info** | GetState removed, TvInfo has all state from GetManagedObjects |
| T6 | **Info** | AutomateWhen gate logic is correct with benefit-of-doubt semantics |
| T7 | **Info** | Daemon-side gate evaluation with negligible lock contention |
| T8 | **Info** | LG input ID normalization fixes tray radio bug |
| T9 | **Info** | Initial subscription state sent immediately |
| T10 | **Info** | Dual logging (stderr + journald) from TTY |
| T11 | **Info** | Session service gates on Automated |
| T12 | **Info** | Tray icon reflects automation gate state |
| T13 | **Low** | `input_app_id_map` on TvState is an LG-specific detail (acceptable) |

## Overall Assessment

This is the most architecturally significant change set in the project's history. The
D-Bus conformance refactor (plan.md) moves the entire state management model from
"fetch on demand through TvManager locks" to "push into D-Bus interface cache, read
instantly." This eliminates:
- Custom signals (StateChanged, ConnectionChanged)
- JSON serialization on the internal path
- Per-TV GetState round-trips from the tray
- 8 dead TvManager getter methods
- The state_change_signal_emitter task

The input-aware automation (plan2.md) builds cleanly on top: the `Automated` D-Bus
property is computed from cached fields in `TvService`, and the session service reads
it from `GetManagedObjects` without additional D-Bus calls. The LG input normalization
fix resolves a long-standing tray radio bug.

One low-severity item: T13 (`input_app_id_map` on `TvState`). Everything else is
info-level confirmation of correct behavior. Both plan checklists are fully executed.

---
---

# D-Bus Conformance Audit (against freedesktop guidelines + zbus book)

Cross-referenced the codebase against the
[D-Bus API Design Guidelines](https://dbus.freedesktop.org/doc/dbus-api-design.html)
and the [zbus book](https://dbus2.github.io/zbus/). Verified via live `busctl introspect`.

---

## U1. Interface versioning — correct

All interfaces follow `org.tisvlay.{Name}1` convention:
`Tv1`, `Tv1.Power`, `Tv1.Screen`, `Tv1.Volume`, `Tv1.Input`, `Tv1.Backlight`, `Manager1`,
`Session1`. Object paths are stable (no version in path), which is correct per spec.

**Severity:** Info (already compliant)

---

## U2. Method naming — correct (zbus auto-converts)

zbus automatically converts Rust `snake_case` to D-Bus `PascalCase`. Verified via
`busctl introspect`: methods appear as `PowerOn`, `AddTv`, `SwitchInput`, etc.
No `#[zbus(name = "...")]` overrides needed.

**Severity:** Info (already compliant)

---

## U3. Property annotations — correct and thorough

Verified via `busctl introspect`:
- `const`: `Id`, `Vendor`, `Address` — never change after registration
- `emits-change`: `Connected`, `Managed`, `Volume`, `Muted`, `Backlight`, `IsScreenOn`,
  `PowerState`, `CurrentInput`, `AutomateWhen`, `Automated`, `PrimaryInput`, etc.
- `emits-invalidation`: `Capabilities`, `AvailableInputs` — complex types, client re-fetches
- `writable`: `Name`, `Managed`, `SystemProfile`, `PrimaryInput`, `Volume`, `Muted`,
  `Backlight`, `IsScreenOn`

All annotations match the spec's recommendations. Writable props have `emits-change`.

**Severity:** Info (already compliant)

---

## U4. Error handling — excellent

Custom `DaemonError` with `#[derive(zbus::DBusError)]` and prefix
`org.tisvlay.Error`. Property setters return `zbus::fdo::Result<()>` (correct — the
spec requires fdo errors for property operations). Methods return `Result<T, DaemonError>`.

**Severity:** Info (already compliant)

---

## U5. Boolean properties — guideline says avoid, pragmatically acceptable

The D-Bus design guidelines state: "Do NOT use boolean properties. Use enumerated
values instead." The codebase has 4 boolean properties:

| Property | Could be enum? | Assessment |
|----------|---------------|------------|
| `Connected` | No | Genuinely binary: connected or not |
| `Managed` | No | Kill switch: on or off |
| `Automated` | No | Computed gate: fires or doesn't |
| `IsScreenOn` | Maybe | Could be `ScreenState { On, Blanked, Off }` |
| `Muted` | No | Audio mute is binary |

The guideline's rationale is future extensibility. For `Connected`, `Managed`, `Muted`,
and `Automated`, a third state is unlikely. `IsScreenOn` could theoretically distinguish
"blanked by user" from "blanked by OLED protection" from "off", but the TV itself
doesn't expose this distinction.

**Severity:** Info (pragmatic deviation, documented rationale)

---

## U6. Profile methods still return JSON strings — spec violation

**Files:**
- `Manager1.GetUserProfileDefault() → s` (JSON string)
- `Manager1.ListSystemProfiles() → a(ss)` (name, JSON pairs)
- `Manager1.ListUserProfiles() → a(ss)` (name, JSON pairs)
- `Session1.GetEffectiveUserProfile(tv_id) → s` (JSON string)
- `Session1.ListUserProfiles() → a(ss)` (name, JSON pairs)

The D-Bus design guideline says: "Sending structured strings over D-Bus should be
avoided. Use the type system (`a{sv}`, `(ss)`, etc.) instead."

Profiles could be returned as `a{sv}` (dict of string → variant), which would let
clients read fields without JSON parsing and enable standard introspection tools to
show profile contents.

However, this is a lower priority than the Phase A-F refactor that was just completed.
The profile data is consumed by three internal clients (session, tray, CLI) that all
parse JSON fine. The main benefit of `a{sv}` would be for external introspection tools
and future third-party consumers.

**Fix direction (when prioritized):** Use `zvariant::SerializeDict` / `DeserializeDict`
to derive `a{sv}` serialization for `SystemProfileConfig` and `UserProfileConfig`,
then change return types from `String` to the dict type.

**Severity:** Low (internal-only consumers, JSON works, typed dicts are better practice)

---

## U7. `SetTvOption` still exists alongside writable properties

**File:** `Manager1.SetTvOption(tv_id, key, value)` — verified on live bus.

The Phase B refactor made `Name`, `Managed`, `SystemProfile`, `PrimaryInput` into
writable properties on per-TV `Tv1` objects. But `SetTvOption` remains on `Manager1`
as a bulk key-value setter that also handles persistence.

The writable property setters on `Tv1` delegate to `set_tv_option` internally, so
both paths work. `SetTvOption` on Manager1 is useful for admin tools that don't know
the TV's object path (they can address by ID), but it's now redundant for clients
that use per-TV properties directly.

This is acceptable — `SetTvOption` serves a different use case (ID-based addressing
from Manager1 vs path-based property set from Tv1). Both are valid D-Bus patterns.

**Severity:** Info (acceptable redundancy for different use cases)

---

## U8. `ConfigReloaded` custom signal — appropriate

`Manager1.ConfigReloaded` is a custom signal with no payload. This is appropriate —
it signals a system-wide metadata change (profiles, TV names, managed flags) that
isn't a property change on Manager1 itself. The D-Bus spec says custom signals are
fine for events that don't map to property changes.

Could add a generation counter or timestamp parameter for clients to detect stale
caches, but the current no-payload design is sufficient since clients do a full refresh
on the signal.

**Severity:** Info (correct use of custom signal)

---

## U9. `UserProfileChanged` custom signal — appropriate

`Session1.UserProfileChanged(tv_id: s, profile_name: s)` is a custom signal. This
is correct — the session service doesn't have per-TV objects, so there's no property
to emit `PropertiesChanged` on. The signal carries the TV ID and new profile name,
which is semantically richer than a generic notification.

**Severity:** Info (correct use of custom signal)

---

## U10. connection::Builder used correctly

Verified in `main.rs`: ObjectManager and Manager1 are registered via `serve_at()`
on the builder before `.build()`. Per-TV interfaces are registered dynamically after
build. The `ManagerService` uses `#[zbus(connection)]` for parameter injection instead
of storing a `Connection` field. Modern zbus pattern.

**Severity:** Info (correct pattern)

---

## U11. Property setters use `&mut self` with async I/O — acceptable

All writable property setters use `&mut self` and call backend methods (WebSocket
round-trip ~20ms) or `set_tv_option` (disk I/O via `spawn_blocking`). The zbus book
warns that `&mut self` holds the interface lock, blocking other property reads/writes
on the same interface.

For this project, the lock duration is acceptable:
- Backend commands complete in ~20ms (fast WebSocket)
- `set_tv_option` uses `spawn_blocking` for disk I/O
- The lock is per-interface (TvService, TvVolumeService, etc.), not per-TV — different
  sub-interfaces can be accessed concurrently
- The tray's scroll coalescing means at most one set per scroll burst

If latency becomes an issue, the fix is to use `&self` with interior mutability
(`Arc<Mutex<>>` around the cached field). Not needed now.

**Severity:** Info (acceptable, documented consideration)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| U1 | **Info** | Interface versioning correct |
| U2 | **Info** | Method naming correct (zbus auto-converts) |
| U3 | **Info** | Property annotations thorough and correct |
| U4 | **Info** | Error handling excellent |
| U5 | **Info** | Boolean props acceptable with documented rationale |
| U6 | **Low** | Profile methods still return JSON — typed `a{sv}` dicts preferred |
| U7 | **Info** | SetTvOption alongside writable props is acceptable redundancy |
| U8 | **Info** | ConfigReloaded custom signal is appropriate |
| U9 | **Info** | UserProfileChanged custom signal is appropriate |
| U10 | **Info** | connection::Builder used correctly |
| U11 | **Info** | Property setters with `&mut self` acceptable for current latencies |

## Overall Assessment

The D-Bus API is well-conformant. The Phase A-F refactor addressed the major deviations
(custom signals, JSON state, manual property reads). The remaining item is **U6**: profile
methods returning JSON strings instead of typed `a{sv}` dicts. This is a low-priority
improvement — internal consumers handle JSON fine, and the refactor requires adding
`zvariant::SerializeDict` / `DeserializeDict` derives to profile types.

The interface design, property annotations, error handling, and signal usage all follow
D-Bus best practices. The live `busctl introspect` output confirms everything is
correctly exposed on the bus.
