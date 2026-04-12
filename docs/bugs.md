# Known Bugs & Issues

## TV ID / D-Bus path inconsistency

**Status**: fixed
**Severity**: medium — latent data-integrity risk, no user-facing symptoms yet
**Found by**: `pair_round_trip` integration tests (Phase C.3)

### Problem

TV IDs and D-Bus object path suffixes could diverge because the D-Bus
spec restricts path components to `[A-Za-z0-9_]`, but TV IDs were not
validated against this alphabet.

The `tv_path()` sanitizer in `tisvlay-core/src/dbus_paths.rs` silently
replaces any character outside `[A-Za-z0-9_]` with `_`. This meant
the mapping from ID to path was **lossy** — multiple distinct IDs
could map to the same path.

### Fix

All five steps from the original fix plan are implemented:

1. TV ID alphabet restricted to `[A-Za-z0-9_]`, validated at config
   load with `ConfigValidation` error and suggested fix
2. Auto-generated IDs use `_` for spaces via `tv_id_from_name()`
3. Duplicate ID detection at config load
4. `AddTv` returns `(ObjectPath, String)` so the client receives the
   canonical ID directly
5. `DaemonClient::pair_tv()` uses the returned ID, never parses paths

---

## `RemoveTv` still derives TV ID from D-Bus path

**Status**: open
**Severity**: low — correct by construction after ID alphabet fix, but fragile
**Found by**: code audit during TV ID fix

### Problem

`tisvlayd/src/dbus/manager.rs:131` extracts the TV ID from the object
path via `strip_prefix("/org/tisvlay/Tv/")`. The client side
(`DaemonClient::remove_tv`) also round-trips through the sanitizer:
ID → `tv_path()` → send path → daemon parses it back.

With the restricted ID alphabet this is now lossless, but it's the
same path → ID derivation pattern we flagged as "never do." If a
future change relaxes the ID alphabet without updating this code, the
bug returns silently.

### Fix plan

Change `RemoveTv` D-Bus method to accept a TV ID string instead of
an object path. The daemon derives the path internally. This
eliminates the round-trip and aligns with the `AddTv` fix.

Note: this changes the D-Bus signature from `o` (object path) to `s`
(string), which changes the introspection XML. No stability guarantees
exist, so this is acceptable, but the proxy trait definition also needs
updating:

```rust
// Before: fn remove_tv(&self, path: ObjectPath<'_>) -> zbus::Result<()>;
// After:  fn remove_tv(&self, tv_id: &str) -> zbus::Result<()>;
```

### Affected files

- `crates/tisvlay-core/src/dbus_client.rs` — `remove_tv()` sends path, proxy trait signature changes
- `crates/tisvlayd/src/dbus/manager.rs` — `RemoveTv` parses path
- `crates/tisvlay-cli/src/main.rs` — calls `client.remove_tv()`

---

## `TvManager::pair_tv` has no timeout on `wait_for_completion`

**Status**: open
**Severity**: medium — can hang a D-Bus call indefinitely
**Found by**: mock_discovery test design (the `drain_pairing` helper
wraps in `tokio::time::timeout` to avoid test hangs)

### Problem

`tisvlayd/src/tv_manager.rs:1216` calls
`session.wait_for_completion().await` with no timeout. If the TV
never responds to the pairing prompt (e.g., TV is powered off mid-pair,
network drops, firmware bug), the D-Bus `AddTv` call blocks forever.
The daemon remains responsive for other TVs but the calling CLI
process hangs.

The core `PairingSession` deliberately omits a built-in timeout
(timeouts belong to the caller), which is the right design. But the
caller (`TvManager`) doesn't apply one.

### Fix plan

Wrap `wait_for_completion()` in `tokio::time::timeout()` with a
configurable deadline (default: 60s for network backends, could be
longer for Bluetooth). Return `PairingError::Timeout` on expiry.

The timeout must also cancel the `PairingSession` so the TV stops
showing the pairing prompt. Without cancel, the TV keeps showing
"Accept?" until its own internal timeout and the user has to dismiss
it manually:

```rust
match tokio::time::timeout(deadline, session.wait_for_completion()).await {
    Ok(result) => result,
    Err(_) => {
        let _ = session.cancel(); // dismiss the TV prompt
        Err(PairingError::timeout())
    }
}
```

### Affected files

- `crates/tisvlayd/src/tv_manager.rs` — `pair_tv()` needs timeout + cancel

---

## Session event handler `last_action` cache survives daemon reconnects

**Status**: open
**Severity**: low — can cause a missed action after daemon restart
**Found by**: session event test design review

### Problem

The session service's event handler tracks the last action sent to
each TV in a `HashMap<String, SessionAction>` to skip redundant
commands. This cache is never invalidated when the daemon disconnects
and reconnects.

If the daemon restarts (or the TV reconnects with a fresh state), the
cache still thinks `ScreenOff` was the last applied action. The next
`SessionLocked` event is skipped as redundant, even though the TV's
screen is actually on after the restart.

### Fix plan

Two options with different granularity:

1. **Coarse (simpler)**: clear the entire `last_action` map on
   `StateEvent::DaemonDisappeared`. Clears all TVs even if only one
   reconnected, but daemon restarts are rare and the cost of
   re-sending one redundant `screen_off` is negligible.

2. **Per-TV (more precise)**: clear a single TV's entry when
   `PropertiesChanged { tv_id }` indicates `Connected` transitioned
   to `true` (fresh connection). The session service already
   subscribes to `StateEvent::DaemonStateChanged` which includes
   `PropertiesChanged { tv_id }` — it would need to check if the
   changed property is `Connected`.

Option 1 is sufficient for the current scale. Option 2 is better if
the project adds many TVs where one reconnects frequently while
others are stable.

### Affected files

- `crates/tisvlay-session/src/lib.rs` — `event_handler()` loop
