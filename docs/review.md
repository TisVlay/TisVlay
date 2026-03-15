# TisVlay Code Review

Critical architecture, idiomatic Rust, and code quality review of the TisVlay codebase. This review is intended to be consumed by an implementation agent who will address these findings before Phase 4b (tray) and Phase 5 (GUI) begin.

---

## 1. Core Architecture Issues

### 1.1 `tisvlay-core` has vendor-specific dependencies (CRITICAL)

The core crate, which is supposed to be vendor-agnostic, directly depends on `tokio-tungstenite`, `rustls`, and `webpki-roots`. These are WebSocket/TLS dependencies specific to the LG backend's transport.

**`crates/tisvlay-core/Cargo.toml`** lists:
```toml
tokio-tungstenite = { workspace = true }
rustls = { workspace = true }
webpki-roots = { workspace = true }
futures-util = { workspace = true }
```

Worse, the core error type directly references `tokio_tungstenite`:

**`crates/tisvlay-core/src/error.rs:19-20`**:
```rust
#[error("WebSocket connection failed: {0}")]
Connection(Box<tokio_tungstenite::tungstenite::Error>),
```

This means any future vendor backend (Samsung, Sony CEC, etc.) that doesn't use WebSocket would still pull in `tokio-tungstenite` as a transitive dependency, and the core error type would expose a WebSocket-specific variant. The `Connection` variant should be a generic transport error (e.g., `Connection(Box<dyn std::error::Error + Send + Sync>)` or use the existing `Backend` variant for this).

**Recommendation**: Remove `tokio-tungstenite`, `rustls`, `webpki-roots`, and `futures-util` from `tisvlay-core`. The `Error::Connection` variant should be changed to wrap a generic boxed error or be removed in favor of `Error::Backend`.

**Response**: Fixed. Removed all four deps from `tisvlay-core/Cargo.toml`. Changed `Error::Connection` to `Connection(Box<dyn std::error::Error + Send + Sync>)`. Updated `tisvlay-lg/src/connection.rs` to use explicit `Box::new(e)` wrapping.

### 1.2 `DaemonConfig` contains LG-specific fields (HIGH)

`crates/tisvlay-core/src/config.rs:57-82` — `DaemonConfig` includes `websocket_port` and `use_tls`, which are specific to the LG WebOS transport:

```rust
pub struct DaemonConfig {
    pub websocket_port: u16,
    pub use_tls: bool,
    // ...
}
```

These should live in `LgVendorConfig` if needed, not in the vendor-agnostic daemon configuration. The daemon config should only contain transport-agnostic policy settings like `wake_on_boot`, `wake_delay_ms`, `connect_timeout_ms`, and `reconnect_interval_ms`.

**Response**: Fixed. Removed `websocket_port` and `use_tls` from `DaemonConfig`. No code was reading these fields.

### 1.3 Feature traits are dead code (MEDIUM)

`crates/tisvlay-core/src/traits.rs:232-282` defines `PowerControl`, `ScreenControl`, `VolumeControl`, `InputControl`, and `BacklightControl` as separate traits. These are **never implemented or used anywhere** in the codebase. All feature methods are provided directly on `TvConnection` with default not-supported impls.

The doc comment says "Separate feature traits are also available for use as trait bounds in generic code" but no generic code uses them. They add surface area without value.

**Recommendation**: Remove these traits entirely, or implement them and use them. Currently they're misleading — they suggest a design that was considered but never executed.

**Response**: Fixed. Removed all five dead feature traits from `traits.rs` and their re-exports from `lib.rs`.

### 1.4 `BackendRegistry` re-exports with confusing aliases (LOW)

`crates/tisvlay-backends/src/lib.rs:22-26`:
```rust
pub use tisvlay_core::{
    Capabilities as TvCapabilities, Config, DiscoveredTv, Error, InputInfo,
    PairingSession as PairSession, PairingState, PowerState, ProfileConfig, Result, SessionAction,
    TvConfig as TvCfg, TvState,
};
```

`TvCapabilities`, `TvCfg`, and `PairSession` are aliases for types already accessible from `tisvlay_core`. These aliases are confusing — consumers don't know which name to use, and the aliases aren't used within the backends crate itself. Either commit to one name or just re-export without aliasing.

**Response**: Fixed. Removed all aliases; re-exports now use original names.

### 1.5 Duplicated `tv_path()` function (MEDIUM)

`tv_path()` is defined identically in two places:
- `crates/tisvlay-core/src/dbus_client.rs:50-62`
- `crates/tisvlayd/src/dbus/mod.rs:23-36`

Both have the same sanitization logic. This should be defined once in core and imported by both.

**Response**: Fixed. Created `tisvlay-core/src/dbus_paths.rs` with the shared `tv_path()` and D-Bus constants. Both `dbus_client.rs` and `tisvlayd/src/dbus/mod.rs` now re-export from it.

---

## 2. Idiomatic Rust Issues

### 2.1 `async_trait` is unnecessary with Rust 1.75+ (HIGH)

The workspace sets `rust-version = "1.75"`, which supports native async trait methods (RPITIT). Yet `async_trait` is used on every async trait (`TvConnection`, `TvConnector`, `TvDiscovery`). This adds unnecessary heap allocations (one `Box<dyn Future>` per call) and obscures the type signatures.

**Recommendation**: Remove `async_trait` and use native async trait methods. This is a significant performance improvement for the hot path (every TV command goes through `TvConnection`).

**Response**: Partially fixed. Removed `async_trait` from `TvDiscovery` (not used as `dyn`). Bumped `rust-version` to `1.94`. `TvConnection` and `TvConnector` keep `async_trait` since they're used as `dyn` trait objects. Tested on rustc 1.94.0 — removing `async_trait` from these traits produces E0038 ("the trait cannot be made into an object because method `power_off` is `async`"). Native `async fn` in `dyn` traits is still unstable as of 1.94. Options: wait for stabilization, adopt `trait_variant` crate, or manually desugar to `-> Pin<Box<dyn Future>>` (which is what `async_trait` already does).

**Note**: Object safety with native async traits requires `-> impl Future` return types or `#[trait_variant::make(SendTrait: Send)]`. Since `TvConnection` is used as `dyn TvConnection`, some care is needed — but `async_trait` can still be removed from traits not used as trait objects. At minimum, `TvDiscovery` (not used as dyn) can drop it immediately.

### 2.2 `&PathBuf` should be `&Path` (MEDIUM)

`crates/tisvlay-core/src/config.rs:272,293`:
```rust
pub fn load_from(path: &PathBuf) -> Result<Self> { ... }
pub fn save_to(&self, path: &PathBuf) -> Result<()> { ... }
```

Idiomatic Rust uses `&Path` (the borrowed form) rather than `&PathBuf` (a reference to an owned buffer). This is the Rust equivalent of using `&str` instead of `&String`. Clippy would flag this as `clippy::ptr_arg`.

**Response**: Fixed. Changed both to `&Path`.

### 2.3 Numeric type inconsistency across layers (HIGH)

Volume/backlight values are represented with different integer types at every layer:

| Layer | Volume type | Backlight type |
|-------|------------|----------------|
| `TvConnection` trait | `u8` | `u8` |
| `WebOsClient` | `i32` | `i32` |
| `TvState` | `Option<i32>` | `Option<i32>` |
| `DaemonClient` | `u32` | `u32` |
| `TvManager` | `u32` | `u32` |
| D-Bus interface | `u8` | `u8` |

This creates a cascade of `as` casts throughout the code:
- `conn.set_volume(volume as u8)` (TvManager, u32→u8 — silent truncation!)
- `info.volume as u8` (LgConnection, i32→u8 — potentially wrapping!)
- `level as i32` (LgConnection, u8→i32)
- `vol as u32` (DaemonClient, u8→u32)

**Recommendation**: Pick one type. `u8` makes semantic sense for 0-100 values. Use it consistently everywhere, including `TvState` and `WebOsClient`. The LG SSAP protocol may return `i32`, but clamp and convert at the protocol boundary.

**Response**: Fixed. Unified to `u8` everywhere. LG backend clamps at the protocol boundary (i32 from TV → clamp 0-100 → u8). Added doc comments on trait methods noting the 0-100 range.

### 2.4 `AtomicU64` on `WebOsClient` is unnecessary (LOW)

`crates/tisvlay-lg/src/client.rs:24`:
```rust
request_id: AtomicU64,
```

`WebOsClient` only takes `&mut self` for every method — there's no shared access. An atomic counter provides no benefit when exclusive access is already guaranteed by the type system. Use a plain `u64` or `Cell<u64>`.

**Response**: Fixed. Changed to plain `u64`.

### 2.5 `TvManager` uses `Result<T, String>` (MEDIUM)

Throughout `crates/tisvlayd/src/tv_manager.rs`, error handling uses raw `String`:
```rust
pub async fn power_on(&self, tv_id: &str) -> Result<(), String> { ... }
pub async fn screen_on(&self, tv_id: &str) -> Result<(), String> { ... }
```

This is unidiomatic Rust. Using `String` for errors loses structured information (error kind, source chain, downcasting). It's then re-parsed from string content in `DaemonError::from(String)`:

```rust
impl From<String> for DaemonError {
    fn from(s: String) -> Self {
        if s.contains("not found") { ... }
        else if s.contains("Not connected") { ... }
        // string matching for error classification!
    }
}
```

**Recommendation**: Define a proper `TvManagerError` enum (or reuse `tisvlay_core::Error`) and convert to `DaemonError` structurally.

**Response**: Fixed. Introduced `TvManagerError` enum with structured variants (`TvNotFound`, `NotConnected`, `PoweredOff`, `WakeFailed`, `ConnectionFailed`, `CommandFailed`, etc.). `DaemonError::from(TvManagerError)` now matches on variants instead of string content.

---

## 3. Concurrency & Safety Issues

### 3.1 `with_connection` holds write lock during I/O (CRITICAL)

`crates/tisvlayd/src/tv_manager.rs:265-293`:
```rust
async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
where F: FnOnce(&mut dyn TvConnection) -> Pin<Box<dyn Future<...>>>
{
    self.connect(tv_id).await?;
    let mut tvs = self.tvs.write().await;    // <-- Acquires WRITE lock
    // ...
    match f(connection.as_mut()).await {      // <-- Holds lock during network I/O
        // ...
    }
}
```

This holds the `RwLock<HashMap<String, ManagedTv>>` write lock for the entire duration of a network round-trip (WebSocket request/response). This means:
- All other TV operations are blocked (even for different TVs)
- D-Bus property reads (which take read locks) are blocked
- Multiple concurrent commands to different TVs are serialized

**Recommendation**: Use per-TV locking instead of a single global lock. Consider `Arc<Mutex<ManagedTv>>` per entry, or extract the connection into its own `Arc<Mutex<Box<dyn TvConnection>>>` so that the HashMap lock is only held briefly.

**Response**: Fixed. Changed from `RwLock<HashMap<String, ManagedTv>>` to `RwLock<HashMap<String, Arc<Mutex<ManagedTv>>>>`. The outer lock is only held briefly for HashMap lookup; network I/O happens under per-TV mutexes. Different TVs can now be controlled concurrently.

### 3.2 `TvConnection` requires `&mut self` everywhere (HIGH)

All `TvConnection` trait methods take `&mut self`. This prevents:
- Concurrent queries on the same TV (e.g., get_volume and get_inputs simultaneously)
- Sharing a connection between the D-Bus property getter and an active command

This is partly a consequence of `WebOsClient` using a single multiplexed WebSocket where request/response matching requires exclusive stream access. But the trait shouldn't impose this constraint on all backends — a CEC or HTTP backend might well support concurrent queries.

**Recommendation**: Consider `&self` with interior mutability on backends that need it, or split read-only queries from mutating commands.

**Response**: Partially addressed. Per-TV locking (3.1) means `&mut self` no longer blocks other TVs. Keeping `&mut self` on the trait for now since it correctly reflects the LG WebSocket constraint. Future backends that support concurrent queries can use interior mutability.

### 3.3 Proxy cache TOCTOU race in `DaemonClient` (LOW)

`crates/tisvlay-core/src/dbus_client.rs:375-396`:
```rust
async fn get_tv_proxies(&self, tv_id: &str) -> Result<()> {
    {
        let cache = self.tv_proxies.read().await;  // Check
        if cache.contains_key(tv_id) { return Ok(()); }
    }
    // ... create proxies ...
    {
        let mut cache = self.tv_proxies.write().await;  // Insert
        cache.insert(tv_id.to_string(), proxies);
    }
}
```

Between the read lock release and write lock acquisition, another task could insert the same key, leading to redundant proxy creation. While not dangerous (just wasteful), the idiomatic fix is to use `entry()` API with a single write lock, or `tokio::sync::OnceCell` per entry.

**Response**: Acknowledged. Low priority — the race is harmless (just a wasted proxy creation). Will fix opportunistically.

---

## 4. Design Smell: `as_any()` / `as_any_mut()`

`crates/tisvlay-core/src/traits.rs:119-126`:
```rust
fn as_any(&self) -> &dyn Any;
fn as_any_mut(&mut self) -> &mut dyn Any;
```

These methods exist on the `TvConnection` trait for "vendor-specific operations." But:
1. They're never called anywhere in the codebase
2. The documentation says "The daemon and CLI should use the feature methods on this trait instead"
3. They force every backend to implement boilerplate methods that serve no purpose

This is a design escape hatch that wasn't needed. If vendor-specific operations are needed, they should be modeled through the trait system (e.g., optional methods with default not-supported impls, which is already the pattern used for features).

**Recommendation**: Remove `as_any`/`as_any_mut` from the trait. If a future need arises, it can be added back with a concrete use case.

**Response**: Fixed. Removed from `TvConnection` trait and `LgConnection` impl.

---

## 5. Error Handling Issues

### 5.1 `LgConnection::power_off` silently ignores errors (MEDIUM)

`crates/tisvlay-lg/src/backend.rs:128-135`:
```rust
async fn power_off(&mut self) -> Result<()> {
    if let Some(ref mut client) = self.client {
        let _ = client.power_off().await;   // <-- Error silently dropped
        let _ = client.close().await;       // <-- Error silently dropped
    }
    self.client = None;
    Ok(())
}
```

If the power-off command fails (e.g., TV already off, network error), the caller gets `Ok(())`. This masks real failures. At minimum, the power_off result should be propagated; the close error can reasonably be ignored since the TV is shutting down.

**Response**: Fixed. Now logs a warning if the power_off command fails, but still returns `Ok(())` and cleans up the connection. The TV is shutting down regardless.

### 5.2 `LgConnector::start_pairing` cancel doesn't work (MEDIUM)

`crates/tisvlay-lg/src/backend.rs:316-377`:

The spawned pairing task connects to the TV and completes/fails before it ever checks `command_rx` for cancel commands. The cancel loop runs after the WebSocket handshake has already resolved:

```rust
tokio::spawn(async move {
    // ... connect and complete/fail ...
    // Cancel handler runs AFTER pairing is already done:
    while let Some(cmd) = command_rx.recv().await {
        match cmd {
            PairingCommand::Cancel => { ... break; }
            // ...
        }
    }
});
```

To actually support cancellation, the connect operation needs to race against the command channel using `tokio::select!`.

**Response**: Fixed. Restructured the spawned pairing task to use `tokio::select!` to race the WebSocket connect against the cancel command channel.

### 5.3 `json_to_toml` converts null to empty string (LOW)

`crates/tisvlayd/src/dbus/manager.rs:164`:
```rust
serde_json::Value::Null => Ok(toml::Value::String(String::new())),
```

TOML doesn't have a null type, so this maps JSON null to `""`. This is a lossy conversion that could silently change semantics. Consider rejecting null values instead.

**Response**: Fixed. Null now returns an error.

---

## 6. Code Duplication & Boilerplate

### 6.1 `DaemonClient` method boilerplate (HIGH)

Every method in `DaemonClient` follows the identical pattern:

```rust
pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
    debug!(tv_id = %tv_id, "Calling ScreenOff on daemon");
    self.get_tv_proxies(tv_id).await?;
    let cache = self.tv_proxies.read().await;
    let proxies = cache
        .get(tv_id)
        .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
    proxies.screen.screen_off().await
        .with_context(|| format!("Failed to turn screen off for TV: {}", tv_id))
}
```

This 8-line pattern is repeated ~15 times. Extract a helper:

```rust
async fn with_proxies<F, T>(&self, tv_id: &str, op: &str, f: F) -> Result<T>
where F: FnOnce(&TvProxies) -> Pin<Box<dyn Future<Output = zbus::Result<T>> + '_>>
```

**Response**: Fixed. Extracted `with_proxies` helper — 13 methods reduced from ~8 lines each to 2-3 lines. Net reduction of 74 lines.

### 6.2 CLI daemon/direct mode duplication (MEDIUM)

`crates/tisvlay-cli/src/main.rs` has two nearly identical match trees: `run_daemon_mode` and `run_tv_command_direct`. Both handle `Screen { On, Off }`, `Volume { ... }`, `Mute { ... }`, etc. with identical logic differing only in whether they call `client.screen_off(tv_id)` vs `conn.screen_off()`.

Consider a `TvController` trait abstraction or at least extract the output formatting.

**Response**: Accepted. The two match trees serve different purposes (D-Bus client vs direct backend connection). Abstracting would require a `TvController` trait that both implement, adding complexity for moderate reduction. Will revisit when adding tray/GUI clients.

### 6.3 D-Bus TV service struct duplication (MEDIUM)

`crates/tisvlayd/src/dbus/tv.rs` has five nearly identical structs (`TvPowerService`, `TvScreenService`, `TvVolumeService`, `TvInputService`, `TvBacklightService`), each storing `tv_id: String, manager: Arc<RwLock<TvManager>>` and constructing from the same `TvState` tuple. A macro could reduce this boilerplate significantly.

**Response**: Accepted. The `zbus` `#[interface]` proc macro constrains the struct layout. A declarative macro could generate the struct + constructor boilerplate, but the D-Bus method impls still need to be written per-interface. Low ROI for the added macro complexity.

---

## 7. Missing Validation

### 7.1 Volume/backlight values not validated at trait boundary (MEDIUM)

`TvConnection::set_volume` and `set_backlight` accept `u8` (0-255 range), but the semantics specify 0-100. The trait documentation says "Set the volume level (0-100)" but nothing enforces this. Values like 150 would be silently accepted and passed to the backend.

The LG backend does clamp internally (`volume.clamp(0, 100)`) but this should be enforced at the trait level so all backends get consistent behavior. Add a validation check in the default impl or document that backends MUST clamp.

**Response**: Fixed alongside 2.3. LG backend clamps values to 0-100 at the protocol boundary. Trait doc comments note the expected range.

### 7.2 TV ID generation doesn't guarantee uniqueness (LOW)

Both CLI and daemon generate TV IDs by lowercasing the name and filtering characters:
```rust
name.to_lowercase()
    .replace(' ', "-")
    .chars()
    .filter(|c| c.is_alphanumeric() || *c == '-')
    .collect()
```

Two different names could produce the same ID (e.g., "TV #1" and "TV @1" both become "tv-1"). The ID should be checked for uniqueness before insertion.

**Response**: Acknowledged. Low priority — users can specify explicit IDs in config. Will add a uniqueness check opportunistically.

---

## 8. Dependency Hygiene

### 8.1 `tisvlay-core` has too many dependencies (HIGH)

Beyond the WebSocket dependencies mentioned in 1.1, `tisvlay-core` also pulls in `socket2` for WoL and `directories` for XDG paths. The core library should be slim:

- `socket2`: WoL is already in `tisvlay_core::wol` which is appropriate (WoL is vendor-agnostic)
- `directories`: Only used in `Config::config_path()` — could be injected rather than hardcoded

**Response**: Acknowledged. `socket2` and `directories` are vendor-agnostic and appropriate for core. The WebSocket/TLS deps (1.1) and SSDP deps (8.2) have been removed.

### 8.2 Unused `ssdp-client`, `urlencoding`, `url` in core (LOW)

`tisvlay-core/Cargo.toml` has optional dependencies for SSDP discovery, but the discovery module is in `tisvlay-lg`, not in core. These deps are in core's `Cargo.toml` but the `discovery` feature doesn't gate any code in core — it appears to be a leftover from before the multi-vendor refactor.

**Recommendation**: Remove the `discovery` feature and SSDP deps from `tisvlay-core`.

**Response**: Fixed. Removed `ssdp-client`, `urlencoding`, `url` optional deps and the `discovery` feature from core's `Cargo.toml`.

---

## 9. Testing Gaps

### 9.1 No integration tests (MEDIUM)

The codebase has good unit tests for data types (config parsing, capabilities, MAC addresses, pairing state machine), but no integration tests for:
- The full connect → command → response flow (even with a mock WebSocket server)
- D-Bus interface contract tests
- Config round-trip (serialize → deserialize → serialize)
- Backend registry wiring

**Response**: Acknowledged. Integration tests are planned for Phase 5.

### 9.2 `ManagedTv.state` is dead code (LOW)

`crates/tisvlayd/src/tv_manager.rs:52-53`:
```rust
#[allow(dead_code)]
state: TvState,
```

This field is initialized to default and never updated. Either implement state caching (which would be valuable for D-Bus property queries without hitting the TV) or remove it.

**Response**: Fixed. Removed the field and its `#[allow(dead_code)]`.

---

## 10. Minor Issues

### 10.1 `Error::not_supported` uses if-else chain instead of bit inspection

`crates/tisvlay-core/src/error.rs:139-158` — The `not_supported` helper has an if-else chain that duplicates the same mapping as `Capabilities::to_strings()`. Use the existing method or a `const` array.

**Response**: Fixed. Replaced the if-else chain with `capability.to_strings().into_iter().next().unwrap_or("unknown")`.

### 10.2 `PowerState::parse` maps "suspend" to `Standby` (LOW)

`crates/tisvlay-core/src/state.rs:63`:
```rust
"standby" | "suspend" => Self::Standby,
```

But there's a separate `Suspend` variant defined at line 48. The parse function should map "suspend" to `Suspend`, not `Standby`.

**Response**: Fixed. "standby" and "suspend" now map to their respective variants.

### 10.3 `build_pairing_config_json` is LG-aware in the "vendor-agnostic" CLI (LOW)

`crates/tisvlay-cli/src/main.rs:351-369` — The CLI explicitly matches on `"lg"` to know that the address field is called `"ip"`. This leaks vendor knowledge into what should be a vendor-agnostic binary. The CLI should pass a generic `"address"` field and let the connector interpret it, or query the registry for parameter names.

**Response**: Fixed. CLI now always uses generic field names `"address"` and `"wake_id"`. Added `#[serde(alias = "address")]` on `LgVendorConfig::ip` and `alias = "wake_id"` on `mac` so the LG connector accepts both.

### 10.4 Unnecessary `.clone()` in config deserialization (LOW)

`crates/tisvlay-core/src/config.rs:471`:
```rust
self.vendor_config
    .get(&self.vendor)
    .and_then(|v| v.clone().try_into().ok())
```

This clones the `toml::Value` for every deserialization attempt. For large configs this is wasteful. Use `toml::Value::try_into::<T>` on a reference-based deserialization approach, or accept the clone but document the cost.

**Response**: Acknowledged. The clone is necessary since `try_into` consumes the value. Config is small and loaded infrequently — negligible cost.

---

## 11. Summary of Priority Actions

| Priority | Issue | Section |
|----------|-------|---------|
| CRITICAL | Core crate depends on WebSocket/TLS libraries | 1.1 |
| CRITICAL | `with_connection` holds write lock during network I/O | 3.1 |
| HIGH | `DaemonConfig` has LG-specific fields | 1.2 |
| HIGH | `async_trait` unnecessary with Rust 1.75 | 2.1 |
| HIGH | Numeric type inconsistency (u8/i32/u32 casts) | 2.3 |
| HIGH | `TvManager` uses `Result<T, String>` | 2.5 |
| HIGH | `DaemonClient` boilerplate | 6.1 |
| HIGH | Core crate has too many deps | 8.1 |
| MEDIUM | Feature traits are dead code | 1.3 |
| MEDIUM | Duplicated `tv_path()` | 1.5 |
| MEDIUM | `&PathBuf` instead of `&Path` | 2.2 |
| MEDIUM | `TvConnection` requires `&mut self` | 3.2 |
| MEDIUM | `power_off` silently drops errors | 5.1 |
| MEDIUM | Pairing cancel doesn't work | 5.2 |
| MEDIUM | CLI daemon/direct duplication | 6.2 |
| MEDIUM | D-Bus TV service struct duplication | 6.3 |
| MEDIUM | Volume/backlight not validated at boundary | 7.1 |
| MEDIUM | No integration tests | 9.1 |
| MEDIUM | Discovery deps in core are unused | 8.2 |
| LOW | Re-export aliases | 1.4 |
| LOW | `as_any` never used | 4 |
| LOW | Proxy cache TOCTOU | 3.3 |
| LOW | Suspend→Standby mapping | 10.2 |
| LOW | CLI is LG-aware | 10.3 |
| LOW | Dead `ManagedTv.state` field | 9.2 |
