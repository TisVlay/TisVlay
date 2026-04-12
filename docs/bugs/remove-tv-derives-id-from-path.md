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
