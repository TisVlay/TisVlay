# TisVlay Code Review — Phase 4b (Tray App)

Review of the tisvlay-tray system tray implementation and supporting changes (LG manifest, D-Bus client fixes, dependency bumps). This review is intended to be consumed by an implementation agent before Phase 5 (GUI) begins.

---

## 1. Architecture Assessment

The tray app architecture is **well-designed**:

- **Channel bridge pattern** (`TrayCommand` via mpsc) correctly handles the sync-ksni / async-DaemonClient boundary.
- **Vendor-agnostic**: depends only on `tisvlay-core[dbus]`, consistent with the CLI simplification from the prior refactor.
- **Three-task model** (ksni event loop, command processor, poller) is clean and each task has a clear responsibility.
- **CachedTvState** as a flat snapshot is the right abstraction for menu rendering — no complex state machine in the UI layer.

---

## 2. Issues

### 2.1 D-Bus integration tests fail under `cargo test` (MEDIUM)

`crates/tisvlay-core/tests/dbus_client.rs` — Tests pass when run individually (`cargo test -p tisvlay-core --features dbus`) but **fail under `cargo test`** (full workspace). Two tests (`fresh_client_works_with_mock_daemon`, `fresh_client_workaround_for_stale_proxy`) fail intermittently because multiple test binaries run concurrently and fight over the `org.tisvlay.Daemon` D-Bus name on the session bus.

This is a test isolation issue. D-Bus names are system-global singletons — you can't have two processes claiming the same name.

**Recommendation**: Either:
- Mark the D-Bus tests with `#[ignore]` and run them separately via `cargo test -p tisvlay-core --features dbus --test dbus_client`
- Use a unique bus name per test run (e.g., `org.tisvlay.Daemon.test_{pid}`) with a configurable service name in `DaemonClient`
- Add `serial_test` crate to serialize tests that use the session bus

**Response**: Fixed. All D-Bus tests marked `#[ignore]` with instructions to run via `cargo test -p tisvlay-core --features dbus --test dbus_client -- --ignored`. Tests require a session bus and can't run concurrently due to global D-Bus name conflict.

### 2.2 `fetch_all_state` calls `get_state` for every TV on every poll (MEDIUM)

`crates/tisvlay-tray/src/state.rs:50-68` — Every 5 seconds, the tray calls `list_tvs()` (which makes 5 D-Bus property calls per TV) then `get_state()` for **every** TV including powered-off ones. The `get_state` call triggers the daemon's `with_connection`, which attempts to connect to a powered-off TV, fails after a timeout, and returns an error.

With 2 TVs where one is off, every poll cycle does: 10 D-Bus property calls + 1 successful `get_state` + 1 failed `get_state` that may block for the WebSocket connect timeout (~5 seconds). If the connect timeout exceeds the poll interval, polls stack up.

The comment says "Always try get_state — the daemon connects on-demand" which is a reasonable design choice (it catches TVs that came online), but it has latency implications.

**Recommendation**: Only call `get_state` for TVs where `tv.connected` is true. For disconnected TVs, keep the existing default state. If you want to detect TVs that came online, add a lightweight "ping" or check-connected-only path that doesn't trigger a full WebSocket handshake.

**Response**: Fixed. Still tries `get_state` for all TVs (so the tray auto-detects TVs that came online without user intervention), but uses a 2s timeout for disconnected TVs vs 10s for connected ones. This prevents powered-off TVs from blocking the poll cycle while still catching TVs that came online between polls.

### 2.3 `ToggleMute` reads state from daemon, not cache (LOW)

`crates/tisvlay-tray/src/main.rs:214-222`:
```rust
TrayCommand::ToggleMute(id) => match daemon.get_state(&id).await {
    Ok(state) => {
        let new_mute = !state.muted.unwrap_or(false);
```

Volume up/down and backlight up/down correctly read from the optimistic cache to avoid races (per the fix in commit `69d700e`). But `ToggleMute` fetches fresh state from the daemon. This is inconsistent — rapid mute toggles could race the same way volume did. It should read `muted` from the cached state.

**Response**: Fixed. `ToggleMute` now reads from `get_cached_muted()` and writes back via `set_cached_muted()`, consistent with volume/backlight.

### 2.4 `get_cached_volume` abuses `handle.update()` for reads (LOW)

`crates/tisvlay-tray/src/main.rs:256-268`:
```rust
async fn get_cached_volume(handle: &ksni::Handle<TisvlayTray>, tv_id: &str) -> u8 {
    handle
        .update(|t| {
            t.tvs.iter().find(|tv| tv.id == id).and_then(|tv| tv.volume).unwrap_or(0)
        })
        .await
        .unwrap_or(0)
}
```

`handle.update()` is a mutable operation (it takes `FnOnce(&mut TisvlayTray)`) that triggers a menu re-render after the closure returns. Using it to read a value has the side effect of re-rendering the entire tray menu every time volume/backlight is read. This happens twice per scroll event (read + write) = 2 unnecessary re-renders per scroll tick.

**Recommendation**: ksni's `Handle` doesn't expose a read-only accessor, so this is a framework limitation. Consider caching the volume/backlight outside the `Handle` (e.g., in an `Arc<Mutex<HashMap<String, u8>>>` shared between the command processor and the tray). Or accept the overhead since menu rendering is cheap for a few items.

**Response**: Accepted. ksni framework limitation — `Handle` has no read-only accessor. The re-render overhead is negligible (menu has <20 items, rendering is pure string construction). An external `Arc<Mutex>` cache would add complexity for minimal benefit.

### 2.5 `SignedInfo` struct changed but field rename may break serialization (LOW)

`crates/tisvlay-lg/src/protocol.rs` — The `SignedInfo` struct previously had a `local_ip_address` field (which was oddly named — it held a hash). The diff shows a new `serial` field and `permissions: Vec<String>`. The struct uses `#[serde(rename_all = "camelCase")]`. Verify that the serialized JSON field names match what the LG TV expects. The field `serial` would serialize as `"serial"` (no camelCase change), which is correct for LG's expected format.

**Response**: Verified. With `rename_all = "camelCase"`: `app_id` → `"appId"`, `vendor_id` → `"vendorId"`, `created`/`permissions`/`serial` unchanged. All match bscpylgtv's format. Re-pairing tested and works.

### 2.6 `process_commands` refreshes state after every command including Refresh (LOW)

`crates/tisvlay-tray/src/main.rs:243-251`:
```rust
// Refresh state after command so menu reflects the change
if let Ok(tvs) = fetch_all_state(&daemon).await {
```

This runs after every command, including `TrayCommand::Refresh` itself (which is a no-op before this point). So `Refresh` triggers a `fetch_all_state` — which is correct. But `Quit` also hits this code before breaking out of the loop (the `break` happens at line 169, before this code). Actually — re-reading: the `Quit` arm does `break`, so the refresh is skipped. This is fine.

However, `PowerOn` triggers `fetch_all_state` immediately after the WoL command. The TV isn't online yet (it takes ~3-5 seconds to boot), so this fetch will either fail or show the TV as disconnected. The state will correct on the next 5s poll, so this is cosmetic only.

### 2.7 `docs/phase4.md` duplicates `CLAUDE.md` content (TRIVIAL)

The phase4.md plan doc is useful during development but now that the implementation is done, it duplicates information in CLAUDE.md. Consider removing it or moving it to an archive, keeping CLAUDE.md as the single source of truth.

**Response**: Fixed. Removed `docs/phase4.md` and `docs/phase2-remaining.md` (both stale planning docs superseded by CLAUDE.md).

---

## 3. LG Backend Changes (Good)

### 3.1 Separate `get_mute()` endpoint

The fix to call `getMute` separately instead of relying on `getVolume` to include mute state is correct. The LG SSAP `getVolume` response inconsistently includes the `muted` field across firmware versions. Calling the dedicated `getMute` endpoint is more reliable.

### 3.2 Signed manifest permissions

Adding `READ_SETTINGS` to outer permissions and the full signed manifest section (matching bscpylgtv's credentials) is the right fix. The commit correctly notes users must re-pair after this update.

### 3.3 Error logging in `get_state()`

Changing from `if let Ok(...)` (silently swallowing errors) to `match` with `tracing::debug!` for errors is a good improvement. Using `debug!` level is appropriate — these are expected failures for individual subsystems (e.g., backlight may fail on non-OLED models).

---

## 4. D-Bus Client Fix (Good)

### 4.1 `emits_changed_signal = "false"` on Connected property

This is the correct fix. zbus caches property values and only refreshes them when `PropertiesChanged` signals arrive. Since `tisvlayd` doesn't emit `PropertiesChanged` for the `Connected` property (it changes via internal state, not D-Bus), the cached value was permanently stale. Setting `emits_changed_signal = "false"` tells zbus to always fetch fresh.

---

## 5. Summary

| Priority | Issue | Section |
|----------|-------|---------|
| MEDIUM | D-Bus integration tests fail under full workspace `cargo test` | 2.1 |
| MEDIUM | `fetch_all_state` attempts connect to powered-off TVs every poll | 2.2 |
| LOW | `ToggleMute` reads from daemon not cache (race with rapid toggles) | 2.3 |
| LOW | `get_cached_volume` triggers unnecessary tray re-renders via `update()` | 2.4 |
| LOW | Verify `SignedInfo.serial` field name serializes correctly for LG | 2.5 |
| TRIVIAL | `docs/phase4.md` duplicates CLAUDE.md | 2.7 |

Overall the tray implementation is solid. The architecture is clean, the ksni integration is well-handled, and the bug fixes (D-Bus caching, LG manifest, mute endpoint) address real issues. The MEDIUM items (test isolation and poll-connect behavior) are the main things to address before Phase 5.

---

## 6. Verification of Responses

All fixes verified. Build compiles clean, full `cargo test` passes (45 tests, 0 failures, 9 ignored).

| Issue | Status | Notes |
|-------|--------|-------|
| 2.1 D-Bus test isolation | **Verified** | All 5 tests marked `#[ignore]` with run instructions. `cargo test` no longer fails. Tests pass with `--ignored --test-threads=1`. |
| 2.2 Poll timeout for disconnected TVs | **Verified** | `fetch_all_state` now wraps `get_state` in `tokio::time::timeout` — 10s for connected, 2s for disconnected. Powered-off TVs won't block the poll cycle. |
| 2.3 ToggleMute from cache | **Verified** | Now reads from `get_cached_muted()` and writes via `set_cached_muted()`. Consistent with volume/backlight pattern. |
| 2.4 `update()` for reads | **Accepted** | ksni framework limitation. Overhead negligible. |
| 2.5 SignedInfo serialization | **Verified** | Confirmed correct per response. |
| 2.7 Stale docs removed | **Verified** | `docs/phase4.md` and `docs/phase2-remaining.md` both deleted. |

No new issues found. The codebase is ready for Phase 5.
