# Known Bugs & Issues

## TV ID / D-Bus path inconsistency

**Status**: fixed  
**Severity**: medium — latent data-integrity risk, no user-facing symptoms yet  
**Found by**: `pair_round_trip` integration tests (Phase C.3)

### Problem

TV IDs and D-Bus object path suffixes can diverge because the D-Bus
spec restricts path components to `[A-Za-z0-9_]`, but TV IDs are not
validated against this alphabet.

The `tv_path()` sanitizer in `tisvlay-core/src/dbus_paths.rs` silently
replaces any character outside `[A-Za-z0-9_]` with `_`. This means
the mapping from ID to path is **lossy** — multiple distinct IDs can
map to the same path:

| Config ID      | D-Bus path suffix |
|----------------|-------------------|
| `living-room`  | `living_room`     |
| `living_room`  | `living_room`     |
| `living.room`  | `living_room`     |

### Consequences

1. **`DaemonClient::pair_tv()` returns the wrong ID.**
   After pairing, the daemon returns the D-Bus object path. The client
   extracts the TV ID by parsing the path suffix (`path.rsplit('/').next()`),
   which gives the sanitized form (`living_room`), not the real ID
   stored in TvManager (`living-room`). A subsequent CLI call using
   the returned ID would fail with "TV not found".

2. **D-Bus path collisions.**
   If a user configures two TVs with IDs `living-room` and
   `living_room`, both map to `/org/tisvlay/Tv/living_room`. The
   second registration silently shadows the first — commands sent to
   one TV go to the other.

3. **Path → ID derivation is fundamentally broken.**
   Any code that recovers a TV ID from a D-Bus path is wrong by
   construction because the mapping is lossy. Today this only affects
   `DaemonClient::pair_tv()`, but it's a trap for future code.

### Why it hasn't bitten anyone yet

- Most TV names produce single-word or underscore-separated IDs
- The CLI defaults to the first TV from `list_tvs()` (which uses the
  real `Tv1.Id` property), so the path-extracted ID is rarely used
- No user has paired a TV with a hyphenated name via the CLI yet
- The project has no deployed user base

### Fix plan

1. **Restrict the ID alphabet** to `[A-Za-z0-9_]`.
   Validate at config load time and reject with a clear error:
   ```
   Error: TV ID 'living-room' contains '-' which is not valid in
   D-Bus object paths. Use 'living_room' instead.
   ```
   This is a breaking change for any config with hyphens, but a
   trivial migration (replace `-` with `_`). Acceptable for a
   greenfield project with no deployed user base.

2. **Auto-generated IDs** (from pairing) use `_` for spaces instead
   of `-`, so they're born valid.

3. **Collision detection** at config load time. Even with the
   restricted alphabet, duplicate IDs should be caught and rejected
   with a clear error naming both colliding entries.

4. **`AddTv` returns the TV ID as a string** alongside the object
   path, so `DaemonClient::pair_tv()` never needs to parse a path.
   The D-Bus method signature changes from returning `o` (object path)
   to `(os)` (object path + ID string), or just `s` (ID string) since
   the path is derivable.

5. **Audit all path → ID derivations.** Grep for `rsplit('/')`
   patterns and replace them with proper ID sources. The rule: ID →
   path is allowed (`tv_path(id)`), path → ID is never allowed.

### Affected files

- `crates/tisvlay-core/src/dbus_client.rs` — `pair_tv()` path parsing (#4, #5)
- `crates/tisvlay-core/src/config.rs` — config load validation (#1, #3)
- `crates/tisvlayd/src/tv_manager.rs` — ID generation in `pair_tv()` (#2)
- `crates/tisvlayd/src/dbus/manager.rs` — `AddTv` return type (#4)
- `crates/tisvlay-core/src/dbus_paths.rs` — `tv_path()` sanitizer docs
- `crates/tisvlayd/tests/pair_round_trip.rs` — update assertions after fix

### Test coverage

The `pair_round_trip::pair_mock_tv_appears_in_tv_list` test documents
the current inconsistency with explicit comments:

```rust
// DaemonClient::pair_tv() extracts the ID from the D-Bus object path
// (where '-' is sanitized to '_'), so it returns "living_room". The
// actual ID stored in TvManager is "living-room", which is what
// list_tvs() reports via the Tv1.Id property.
assert_eq!(tv_id, "living_room");  // path-derived (wrong)
assert_eq!(tvs[0].id, "living-room");  // property-derived (correct)
```

After the fix, both should return the same value.
