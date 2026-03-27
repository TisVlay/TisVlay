# Refactor: Transport-agnostic wake and consistent daemon lifecycle config

## Motivation

Two related issues in the current codebase:

1. **Transport leakage in `wake_and_connect`**: The `TvConnector` trait exposes
   `boot_delay`, `max_attempts`, and `retry_interval` parameters that are
   fundamentally transport-specific. The daemon's `wake_delay_ms` config option
   encodes LG WebSocket timing assumptions into the vendor-agnostic layer.
   A USB or RS232 backend would have no use for any of these.

2. **Inconsistent daemon lifecycle config**: `wake_on_boot` is a boolean while
   `on_shutdown` uses the `SessionAction` enum. There is no way to distinguish
   reboot from poweroff. The `on_no_active_session` naming is verbose and
   collides with the "active input" terminology used elsewhere.

## Design decisions

- No backward compatibility constraints. No releases, no public users.
  Old config shapes, field names, comments, and docs are replaced wholesale.
- Any lifecycle event can trigger any `SessionAction` (power_on, power_off,
  screen_on, screen_off, none). The system is not opinionated about which
  actions are valid for which events.

---

## Part 1: `wake_and_connect` signature cleanup

### 1.1 Remove timing params from `TvConnector::wake_and_connect()`

**File:** `crates/tisvlay-core/src/traits.rs`

Change signature from:
```rust
async fn wake_and_connect(
    &self,
    vendor_config: &toml::Value,
    boot_delay: Duration,
    max_attempts: u32,
    retry_interval: Duration,
) -> Result<Box<dyn TvConnection>>;
```
To:
```rust
async fn wake_and_connect(
    &self,
    vendor_config: &toml::Value,
) -> Result<Box<dyn TvConnection>>;
```

Remove the default trait implementation entirely. Each backend is responsible
for its own retry strategy.

### 1.2 Update LG backend

**File:** `crates/tisvlay-lg/src/backend.rs`

- Update `wake_and_connect` to match new signature
- Remove the initial `boot_delay` sleep — start connecting immediately
- Hardcode retry constants as `const` in the backend module:
  - `WAKE_CONNECT_MAX_ATTEMPTS: u32 = 10`
  - `WAKE_CONNECT_RETRY_INTERVAL: Duration = Duration::from_secs(3)`
- The WebSocket connection timeout provides natural pacing; the retry loop
  handles the TV boot delay organically

### 1.3 Update `BackendRegistry`

**File:** `crates/tisvlay-backends/src/lib.rs`

- Remove `wake_delay_ms` parameter from `wake_and_connect_tv()`
- Just call `connector.wake_and_connect(vendor_config).await`

### 1.4 Remove `wake_delay_ms` from daemon config

**Files:**
- `crates/tisvlay-core/src/config.rs` — remove field from `DaemonConfig`,
  remove `default_wake_delay()`
- `crates/tisvlayd/src/tv_manager.rs` — remove `wake_delay_ms` reads from
  `boot_wake_tvs()` and `power_on()`; pass no timing to registry
- `crates/tisvlayd/src/dbus/manager.rs` — remove `wake_delay_ms` from
  `SetDaemonOption` handling
- `crates/tisvlay-core/src/dbus_client.rs` — remove any `wake_delay_ms`
  references
- `docs/configs/daemon.toml` — remove field

---

## Part 2: Daemon lifecycle config consistency

### 2.1 Replace `wake_on_boot` with `on_startup`

**File:** `crates/tisvlay-core/src/config.rs`

Replace:
```rust
pub wake_on_boot: bool,
```
With:
```rust
pub on_startup: SessionAction,  // default: PowerOn
```

### 2.2 Add `on_reboot`

**File:** `crates/tisvlay-core/src/config.rs`

Add:
```rust
pub on_reboot: SessionAction,  // default: None
```

Default `None` matches regular monitor behavior — TV stays in whatever state
it was in during reboot so the user can see BIOS, boot TTY, etc.

### 2.3 Rename `on_no_active_session` to `on_no_session`

**Files:**
- `crates/tisvlay-core/src/config.rs` — rename field
- `crates/tisvlayd/src/tv_manager.rs` — rename `DaemonAction::NoActiveSession`
  to `DaemonAction::NoSession`; update `get_daemon_config_action()`
- `crates/tisvlayd/src/logind.rs` — update references
- `crates/tisvlayd/src/dbus/manager.rs` — update `SetDaemonOption` key

### 2.4 Detect reboot vs poweroff via logind

**File:** `crates/tisvlayd/src/logind.rs`

In the `PrepareForShutdown` handler:
1. Add `ScheduledShutdown` property to the `Login1Manager` proxy:
   returns `(String, u64)` — `(type, usec_timestamp)`
2. When `PrepareForShutdown(true)` fires, query `ScheduledShutdown`
3. Map shutdown type to daemon action:
   - `"poweroff"` | `"halt"` → `DaemonAction::Shutdown`
   - `"reboot"` | `"kexec"` → `DaemonAction::Reboot`
   - anything else → `DaemonAction::Shutdown` (safe fallback)
4. Add `DaemonAction::Reboot` variant and wire it to `on_reboot` config

### 2.5 Update `on_startup` implementation

**File:** `crates/tisvlayd/src/tv_manager.rs`

Replace the `boot_wake_tvs()` logic:
- Currently: if `wake_on_boot`, call `wake_and_connect_tv` + screen_on + input switch
- New: read `on_startup` action; call `execute_action_all_tvs` with the action
- `PowerOn` action triggers WoL + connect + screen_on + input switch (same as before)
- `ScreenOn` action just turns screen on (for TVs already connected)
- `None` action skips boot wake entirely

### 2.6 Update `SetDaemonOption` keys

**File:** `crates/tisvlayd/src/dbus/manager.rs`

Supported keys become:
- `on_startup` (replaces `wake_on_boot`)
- `on_shutdown`
- `on_reboot` (new)
- `on_no_session` (replaces `on_no_active_session`)

Remove `wake_delay_ms` and `wake_on_boot` keys.

---

## Part 3: Verify unified action model

All lifecycle events (both daemon-level and profile-level) already use the
`SessionAction` enum which has `None`, `ScreenOff`, `ScreenOn`, `PowerOff`,
`PowerOn`. Verify that no code artificially restricts which actions are
valid for which events. Specifically check:

- `execute_action_all_tvs()` in logind.rs — already handles all variants
- Session monitor event handlers — already dispatch on full enum
- Config parsing — serde derives should accept any variant for any field

No code changes expected here, just verification.

---

## Part 4: Audit for other transport leaks

Scan the daemon and core crates for any remaining assumptions that leak
vendor or transport details into vendor-agnostic code. Known clean areas:
- `TvConnector::connect()` takes only `&toml::Value` (clean)
- `TvConnection` methods are feature-based, not transport-based (clean)

Check for:
- Any remaining IP/MAC/WebSocket references in daemon or core code
- Any hardcoded port numbers or protocol assumptions outside vendor crates
- Any config fields that encode transport knowledge

---

## Part 5: Update all docs, configs, and tests

- `docs/design.md` — update DaemonConfig schema, config examples, phase
  descriptions, CLI reference, trait diagrams
- `docs/configs/daemon.toml` — update to new field names
- `.claude/CLAUDE.md` — update config examples and busctl examples
- `crates/tisvlay-core/tests/` — update test fixtures
- Any integration test TOML fixtures

---

## New daemon config shape

```toml
[daemon]
on_startup = "power_on"       # action when daemon starts (default: power_on)
on_shutdown = "power_off"     # action on poweroff/halt (default: power_off)
on_reboot = "none"            # action on reboot/kexec (default: none)
on_no_session = "none"        # action when all user sessions end (default: none)
```

## Checklist

- [x] Part 1.1: Remove timing params from `TvConnector::wake_and_connect()`
- [x] Part 1.2: Update LG backend (no boot_delay, hardcoded constants)
- [x] Part 1.3: Update `BackendRegistry::wake_and_connect_tv()`
- [x] Part 1.4: Remove `wake_delay_ms` from all layers
- [x] Part 2.1: Replace `wake_on_boot` with `on_startup`
- [x] Part 2.2: Add `on_reboot`
- [x] Part 2.3: Rename `on_no_active_session` → `on_no_session`
- [x] Part 2.4: Detect reboot vs poweroff via logind `ScheduledShutdown`
- [x] Part 2.5: Update `on_startup` implementation (replace `boot_wake`)
- [x] Part 2.6: Update `SetDaemonOption` keys
- [x] Part 3: Verify no code restricts action-event combinations (clean)
- [x] Part 4: Audit for remaining transport leaks (clean)
- [x] Part 5: Update docs, configs, tests (done inline with Parts 1–2)
- [x] `cargo check` all crates
- [x] `cargo test` all crates
