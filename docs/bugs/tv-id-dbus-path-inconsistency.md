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
