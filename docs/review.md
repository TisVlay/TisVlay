# Review: End-to-end push-based state updates (8a0f817..ea13bb1)

Review of 20 commits implementing the full push-based state pipeline:
TV → WebSocket push → LG backend → daemon StateChangeEvent → D-Bus signal → tray.

Build: clean. Tests: all pass (3 passed, 4 ignored, 1 doc-test ignored).

---

## Q1. Connection multiplexing is well-designed

**File:** [crates/tisvlay-lg/src/connection.rs](crates/tisvlay-lg/src/connection.rs)

The synchronous request/response model is replaced with a multiplexed design:
- Writer: `Arc<Mutex<SplitSink>>` shared between callers and reader task
- Reader: background tokio task routes by request ID via `PendingMap`
- Unmatched messages (subscription updates) go to `mpsc::UnboundedReceiver`
- `Drop` aborts the reader task to prevent leaked tasks

The `PendingMap` uses `std::sync::Mutex` (not tokio) since it's only held
briefly for `insert`/`remove` — correct choice for non-async lock bodies.

The `request()` method registers the waiter before sending (line 244-245) to
avoid a race where the response arrives before the waiter is registered. On
timeout, it cleans up the stale waiter (line 260). On send failure, it removes
the waiter (line 250). All three paths are handled correctly.

**Severity:** Info (well-implemented)

---

## Q2. Subscription routing by request ID — correct design

**File:** [crates/tisvlay-lg/src/backend.rs:240-275](crates/tisvlay-lg/src/backend.rs#L240-L275)

SSAP subscription updates carry the original request ID but no URI field.
The `subscribe_state_updates()` returns a `HashMap<request_id, uri>` mapping.
The converter task uses this to identify which endpoint each update belongs to.

This was the critical insight from the live probing (documented in commit b33b25a) —
the initial implementation matched on `response.uri` which was always `None`,
silently dropping all subscription updates.

**Severity:** Info (correct fix for undocumented SSAP behavior)

---

## Q3. `update_state_from_subscription` — comprehensive parsing

**File:** [crates/tisvlay-lg/src/backend.rs:280-360](crates/tisvlay-lg/src/backend.rs#L280-L360)

Handles all subscription formats correctly:
- Volume: supports both flat `{"volume": N}` and nested `{"volumeStatus": {"volume": N, "muteStatus": bool}}`
- Backlight: uses synthetic URI `#picture/backlight` to distinguish from other `getSystemSettings` calls
- Power state: delegates to `PowerState::parse()` for transitional states
- Inputs: parses the `devices` array with connected status

One observation: the `uri.contains()` matching (e.g., `uri.contains("getPowerState")`)
is brittle if a future endpoint name contains one of these substrings. But since
SSAP endpoint URIs are well-namespaced (`ssap://com.webos.service.tvpower/power/getPowerState`),
collisions are unlikely. A match on exact URI suffixes would be more robust but
isn't necessary given the stable SSAP namespace.

**Severity:** Info (acceptable for the known endpoint set)

---

## Q4. `try_subscribe_state` called at all connection sites — thorough

**File:** [crates/tisvlayd/src/tv_manager.rs](crates/tisvlayd/src/tv_manager.rs)

Subscription is attempted at every point where a connection is established:
- `with_connection` (on-demand reconnect) — line 643
- `reconnect` — line 690
- `power_on` (WoL) — line 752
- `startup_power_on` (boot wake) — line 483
- `pair_tv` (new pairing) — line 1006

No connection site is missed. If the backend doesn't support subscriptions,
`try_subscribe_state` silently returns (line 339-341).

**Severity:** Info (thorough coverage)

---

## Q5. `StateChangeEvent` conversion to enum — good refactor

**File:** [crates/tisvlayd/src/tv_manager.rs:69-79](crates/tisvlayd/src/tv_manager.rs#L69-L79)

Changed from a struct (with `tv_id` and `state_json`) to an enum with
`TvStateChanged` and `ConfigReloaded` variants. The old approach emitted
reload events on `tv_id="_all"` which produced an invalid D-Bus object path.
`ConfigReloaded` now emits on `Manager1` instead of a per-TV path.

New constructors `command_completed()` and `option_change()` are concise and
consistent. Every write command now emits a signal, which means the tray
refreshes after any CLI/D-Bus command without waiting for the fallback poll.

**Severity:** Info (correct design)

---

## Q6. Signal emission after every write command — slight concern about volume

**File:** [crates/tisvlayd/src/tv_manager.rs:811-915](crates/tisvlayd/src/tv_manager.rs#L811-L915)

Every `set_volume`, `set_mute`, `screen_on`, `screen_off`, `switch_input`,
`set_backlight` now emits `StateChangeEvent::command_completed()`. Combined
with SSAP subscription updates (which also emit `StateChangeEvent`), a single
`set_volume` call produces two signals: one from the command completion and
one from the TV's subscription push.

The tray debounces within 100ms, so these two signals are coalesced into one
refresh. This is fine for the current signal volume. If rapid programmatic
commands are added in the future (e.g., fade backlight from 100→0 in steps),
the debounce window may need tuning.

**Severity:** Info (acceptable, debounce handles the dual-signal case)

---

## Q7. Tray `watch_state` — clean tokio::select! with three branches

**File:** [crates/tisvlay-tray/src/main.rs:179-285](crates/tisvlay-tray/src/main.rs#L179-L285)

The `tokio::select!` cleanly handles:
1. D-Bus signal → debounce 100ms → refresh
2. Session service appeared/disappeared → connect/clear → refresh
3. Fallback poll at 30s

The debounce implementation (lines 220-230) drains additional signals within the
window using a nested `tokio::select!` with `biased` — correct: the signal branch
is checked first so no signals are missed during the drain.

The fallback poll interval increased from 5s to 30s, which is appropriate now that
signals provide sub-second updates. The 30s poll handles lazy session service
reconnection and resilience against missed signals.

**Severity:** Info (correct async design)

---

## Q8. `watch_service()` utility — clean extraction

**File:** [crates/tisvlay-core/src/dbus_client.rs](crates/tisvlay-core/src/dbus_client.rs)

The `NameOwnerChanged` subscription pattern is extracted from the tray into a
shared `watch_service()` function returning `Stream<ServiceEvent>`. The tray
now calls `watch_service(conn, "org.tisvlay.Session")` instead of directly
wiring `DBusProxy` and `receive_name_owner_changed_with_args`.

Reusable by the future GUI for discovering both daemon and session services.
The `ServiceEvent` enum (`Appeared`/`Disappeared`) is more ergonomic than
a raw signal with nullable fields.

**Severity:** Info (good reusable abstraction)

---

## Q9. `BTreeMap` for profiles — correct fix for tray submenu collapse

**File:** [crates/tisvlay-core/src/config.rs](crates/tisvlay-core/src/config.rs)

`SystemSection.profiles` and `UserSection.profiles` changed from `HashMap` to
`BTreeMap`. This ensures deterministic iteration order at the source, eliminating
the false state-change detections in the tray's shadow cache that caused
unnecessary `ksni::Handle::update()` calls and submenu collapses.

The `PartialEq` derive on `CachedTvState` relies on `Vec<String>` comparison
for profile lists — with `BTreeMap` providing stable ordering, the tray no
longer needs per-site sort workarounds.

Tests in `state.rs` verify that same-order profile lists compare equal and
different-order lists compare unequal (demonstrating the pre-fix bug).

**Severity:** Info (correct root-cause fix)

---

## Q10. Session `UserProfileChanged` signal — write lock dropped before emit

**File:** [crates/tisvlay-session/src/session_dbus.rs:146-154](crates/tisvlay-session/src/session_dbus.rs#L146-L154)

```rust
// Drop the write lock before emitting the signal
drop(s);

// Notify subscribers that the profile changed
if let Err(e) = Self::user_profile_changed(&ctxt, tv_id, profile_name).await {
```

Correctly drops the `RwLockWriteGuard` before emitting the D-Bus signal. Signal
emission may yield, and holding a write lock across a yield would block all
concurrent reads. Good async discipline.

**Severity:** Info (correct lock discipline)

---

## Q11. `subscribe_state_changes` match rules — AddMatch is low-level

**File:** [crates/tisvlay-core/src/dbus_client.rs:500-560](crates/tisvlay-core/src/dbus_client.rs#L500-L560)

The signal subscription uses raw `AddMatch` calls to `org.freedesktop.DBus`
rather than zbus's higher-level `MessageStream` filtering. This is correct
for the use case (broad match on multiple interfaces and paths) but has a
subtlety: the match rules are added but never explicitly removed. On
`DaemonClient` drop, the bus connection closes and the rules are implicitly
removed. For a long-lived client like the tray this is fine, but if
`subscribe_state_changes` were called multiple times, match rules would
accumulate.

Since `subscribe_state_changes` is called exactly once at tray startup,
this is not a practical concern.

**Severity:** Info (correct for single-subscription usage)

---

## Q12. Plan checklist is fully complete

All 18 checklist items in `docs/plan.md` are marked `[x]`. The plan's
"tentative Part 7" (backlight polling) was made unnecessary by the discovery
that backlight IS subscribable via `ssap://settings/getSystemSettings` with
`category: "picture", keys: ["backlight"]`. All TV state is now fully push-based.

**Severity:** Info (plan fully executed)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| Q1 | **Info** | Connection multiplexing is well-designed |
| Q2 | **Info** | Subscription routing by request ID is correct |
| Q3 | **Info** | Subscription parsing is comprehensive |
| Q4 | **Info** | `try_subscribe_state` called at all connection sites |
| Q5 | **Info** | `StateChangeEvent` enum refactor is clean |
| Q6 | **Info** | Dual signals from command + subscription handled by debounce |
| Q7 | **Info** | Tray `watch_state` async design is correct |
| Q8 | **Info** | `watch_service()` is a clean reusable abstraction |
| Q9 | **Info** | `BTreeMap` is the correct root-cause fix for submenu collapse |
| Q10 | **Info** | Write lock dropped before signal emission |
| Q11 | **Info** | Match rules correctly scoped for single-subscription usage |
| Q12 | **Info** | Plan fully executed, all items checked off |

## Overall Assessment

This is the largest and most impactful refactor in the branch — converting the entire
state pipeline from poll-based to push-based. The implementation is thorough:

- **LG backend**: WebSocket multiplexing with request/response correlation via oneshot
  channels and subscription routing by request ID. Clean split between reader task and
  shared writer. Drop impl prevents leaked tasks.
- **Daemon**: `try_subscribe_state` at every connection site, `StateChangeEvent` enum
  with `ConfigReloaded` variant, signal emission after every write command.
- **Tray**: signal-driven with 100ms debounce, NameOwnerChanged for session discovery,
  30s fallback poll for resilience. BTreeMap ordering eliminates shadow cache false
  positives.
- **Session**: `UserProfileChanged` signal with correct lock discipline.

No actionable items. All findings are info-level confirmations of correct behavior.
The live probing documentation in `docs/lg/webos.md` and probe scripts in `scripts/`
are valuable reference material for future SSAP work.

---
---

# Follow-on Review: Initial state seeding, log level, session signals (1869201..22beed4)

Review of 3 implementation commits following the Q1–Q12 review.

Build: clean. Tests: all pass.

---

## R1. Initial state seeding from subscribe responses — correct fix

**Files:**
- [crates/tisvlay-lg/src/client.rs:153-224](crates/tisvlay-lg/src/client.rs#L153-L224)
- [crates/tisvlay-lg/src/backend.rs:244-268](crates/tisvlay-lg/src/backend.rs#L244-L268)

`subscribe_state_updates()` now returns `(HashMap<id, uri>, Vec<(uri, Response)>)` —
the ID→URI map plus the raw initial responses. The converter task seeds its cached
`TvState` by running each initial response through `update_state_from_subscription`
before entering the receive loop.

This prevents the first push update from producing a partial snapshot (e.g., a volume
change arriving with all other fields at their `TvState::new()` defaults). The initial
responses from SSAP subscribe calls contain the current values, so the seeded state
is complete from the start.

**Severity:** Info (correct fix for incomplete initial state)

---

## R2. Converter exit log downgraded to trace — appropriate

**File:** [crates/tisvlay-lg/src/backend.rs:287](crates/tisvlay-lg/src/backend.rs#L287)

The "Subscription state converter task exited" message fires during every startup
because `on_startup` replaces the initial connection, dropping the first subscription
channel. This is expected behavior, not an error. Downgrading from `debug!` to `trace!`
reduces noise in normal operation while keeping the message available for detailed
troubleshooting.

**Severity:** Info (correct log level)

---

## R3. Session `UserProfileChanged` subscription in tray — correct approach

**File:** [crates/tisvlay-tray/src/main.rs:220-260](crates/tisvlay-tray/src/main.rs#L220-L260)

The tray adds an `AddMatch` rule for `org.tisvlay.Session1.UserProfileChanged` on
the session bus at startup. The match rule is unconditional — it silently receives
nothing while the session service isn't running, and picks up signals when it starts.
This avoids the need to add/remove the rule when the session service appears/disappears.

The signal is handled as a fourth `tokio::select!` branch that triggers a
`refresh_state()`. No debounce — profile changes are rare enough that each one
warrants an immediate refresh.

One observation: the `AddMatch` + `MessageStream` + `filter_map` pattern here
duplicates the approach in `subscribe_state_changes()` on `DaemonClient`. If the
session service grows more signals, this could be extracted into a similar
`subscribe_session_changes()` method. For a single signal, inline is fine.

**Severity:** Info (correct, extraction optional for now)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| R1 | **Info** | Initial state seeding prevents incomplete first snapshot |
| R2 | **Info** | Converter exit log correctly downgraded to trace |
| R3 | **Info** | Session signal subscription is unconditional and correct |

## Overall Assessment

All three commits are clean fixes that refine the push-based pipeline. The initial
state seeding (R1) is the most important — without it, the first D-Bus StateChanged
signal after connection would carry an incomplete TvState. No actionable items.

---
---

# Follow-on Review: D-Bus signal subscription utilities (cf25bc5)

Review of 1 commit extracting shared D-Bus signal helpers, directly addressing
the R3 observation about duplicated `AddMatch` + `MessageStream` pattern.

Build: clean. Tests: all pass.

---

## S1. `add_signal_match` and `subscribe_signals` — clean extraction

**File:** [crates/tisvlay-core/src/dbus_client.rs:250-283](crates/tisvlay-core/src/dbus_client.rs#L250-L283)

Two utilities at the right granularity:

- `add_signal_match(conn, rule)` — adds a match rule only. Used by
  `DaemonClient::subscribe_state_changes()` which needs three rules
  sharing one `MessageStream`.
- `subscribe_signals(conn, rule)` — adds rule + returns `MessageStream`.
  Used by the tray for the single session `UserProfileChanged` subscription.

Both exported via `tisvlay-core::lib.rs` for GUI reuse.

**Severity:** Info (correct extraction)

---

## S2. Tray simplified — interface check removed

**File:** [crates/tisvlay-tray/src/main.rs:232-241](crates/tisvlay-tray/src/main.rs#L232-L241)

The tray's `filter_map` now checks only `member == "UserProfileChanged"`,
dropping the redundant `iface == "org.tisvlay.Session1"` check. This is safe
because the match rule already constrains the interface — only matching signals
reach the stream. Less code, same filtering.

**Severity:** Info (correct simplification)

---

## S3. `DaemonClient` uses `add_signal_match` for its three rules

The three `call_method("AddMatch", ...)` blocks are replaced with
`add_signal_match(conn, &format!(...))`. Same behavior, ~15 fewer lines,
and the error context is generated by the helper. The shared `MessageStream`
is created after all three rules are added, same as before.

**Severity:** Info (clean deduplication)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| S1 | **Info** | Two utilities at correct granularity, exported for GUI |
| S2 | **Info** | Tray filter simplified, match rule handles interface constraint |
| S3 | **Info** | DaemonClient deduplication is clean |

No actionable items. This directly addresses the R3 observation.
