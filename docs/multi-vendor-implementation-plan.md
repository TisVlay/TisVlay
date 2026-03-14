# tisvlay Multi-Vendor Refactor Implementation Plan

This document outlines the phased implementation plan for refactoring tisvlay to support multiple TV vendors.

See [multi-vendor-design.md](multi-vendor-design.md) for the full architecture and design specification.

---

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | Core Abstractions | ✅ Complete |
| 2 | LG Backend Extraction | ✅ Complete |
| 3 | Daemon Refactor | ✅ Complete |
| 4 | CLI & Session Service | ✅ Complete |
| 4.5 | Abstraction Cleanup | 🔄 In Progress |
| 5 | Testing & Documentation | ⏳ Pending |

---

## Phase 1: Core Abstractions (Foundation) ✅

**Status**: Complete

**Goal**: Define traits and types without breaking existing functionality.

### Tasks

1. Create `tisvlay-core/src/traits.rs` with all trait definitions
   - `TvBackend` - core backend trait
   - `TvConnection` - connection to a specific TV
   - `PowerControl`, `ScreenControl`, `VolumeControl`, `InputControl`, `BacklightControl` - feature traits
   - `TvConnector` - factory for creating connections
   - `TvDiscovery` - network discovery trait

2. Create `tisvlay-core/src/capabilities.rs` with Capabilities bitflags
   - `POWER`, `SCREEN`, `VOLUME`, `MUTE`, `INPUT`, `BACKLIGHT`, `WOL`, `DISCOVERY`
   - `to_strings()` method for D-Bus compatibility

3. Update `tisvlay-core/src/config.rs` with new profile-based schema
   - Add `ProfileConfig` struct
   - Update `TvConfig` with `vendor`, `profile`, `hdmi_input`, `only_when_active`
   - Add profile resolution logic

4. Add `tisvlay-core/src/pairing.rs` with PairingSession state machine
   - `PairingState` enum: `AwaitingTvPrompt`, `AwaitingUserInput`, `Complete`, `Failed`
   - `InputType` enum: `None`, `Pin`, `Password`
   - Channel-based communication for async pairing flow

5. Update `tisvlay-core/src/error.rs` with backend-agnostic errors
   - Add `NotSupported` error for missing capabilities
   - Add `BackendError` wrapper for vendor-specific errors

### Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tisvlay-core/src/traits.rs` | Create |
| `crates/tisvlay-core/src/capabilities.rs` | Create |
| `crates/tisvlay-core/src/pairing.rs` | Create |
| `crates/tisvlay-core/src/config.rs` | Modify |
| `crates/tisvlay-core/src/error.rs` | Modify |
| `crates/tisvlay-core/src/lib.rs` | Update exports |
| `crates/tisvlay-core/Cargo.toml` | Add `bitflags`, `async-trait` |

### Verification

```bash
cargo build -p tisvlay-core
cargo test -p tisvlay-core
```

### Completion Notes

- Created `traits.rs` with `TvBackend`, `TvConnection`, and feature traits
- Created `capabilities.rs` with `Capabilities` bitflags
- Updated `config.rs` with profile-based schema (`ProfileConfig`, `LgVendorConfig`)
- Added `get_profile_for_tv()` method for profile resolution
- Vendor-specific config now nested under `[tv.lg]` section
- Legacy `client_key` and `default_input` fields removed from `TvConfig` (no backwards compatibility)

---

## Phase 2: LG Backend Extraction ✅

**Status**: Complete

**Goal**: Move LG-specific code to `tisvlay-lg` crate.

### Tasks

1. Create `crates/tisvlay-lg/` crate structure
   - Set up Cargo.toml with dependencies from tisvlay-core
   - Create module structure

2. Move LG-specific files from tisvlay-core to tisvlay-lg:
   - `client.rs` → WebOsClient implementation
   - `connection.rs` → WebSocket connection with rustls
   - `protocol.rs` → SSAP message types
   - `endpoints.rs` → ssap:// URI constants

3. Move LG-specific discovery code to tisvlay-lg
   - SSDP discovery with LG-specific filtering
   - DLNADeviceName.lge.com header parsing

4. Implement traits for LG backend:
   - `LgBackend` implementing `TvBackend`
   - `LgConnection` implementing `TvConnection` + all feature traits
   - `LgConnector` implementing `TvConnector`
   - `LgDiscovery` implementing `TvDiscovery`

5. Create `LgConfig` struct in tisvlay-lg
   - `client_key: Option<String>`
   - `default_input: Option<String>`

6. Update tisvlay-core to remove LG-specific code
   - Remove direct LG dependencies
   - Update re-exports

### Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tisvlay-lg/Cargo.toml` | Create |
| `crates/tisvlay-lg/src/lib.rs` | Create |
| `crates/tisvlay-lg/src/backend.rs` | Create |
| `crates/tisvlay-lg/src/client.rs` | Move from core |
| `crates/tisvlay-lg/src/connection.rs` | Move from core |
| `crates/tisvlay-lg/src/protocol.rs` | Move from core |
| `crates/tisvlay-lg/src/endpoints.rs` | Move from core |
| `crates/tisvlay-lg/src/discovery.rs` | Move from core |
| `crates/tisvlay-lg/src/config.rs` | Create |
| `crates/tisvlay-core/src/lib.rs` | Remove LG exports |
| `Cargo.toml` (workspace) | Add tisvlay-lg member |

### Verification

```bash
cargo build -p tisvlay-lg
cargo test -p tisvlay-lg

# Ensure core still builds without LG code
cargo build -p tisvlay-core
```

### Completion Notes

- Created `tisvlay-lg` crate with LG-specific implementation
- Moved `client.rs`, `connection.rs`, `protocol.rs`, `endpoints.rs`, `discovery.rs` from core
- LG code remains functionally as `WebOsClient` (trait implementations deferred to future work)
- `LgVendorConfig` defined in `tisvlay-core/src/config.rs` for now
- Core re-exports LG types for backwards compatibility during transition

---

## Phase 3: Daemon Refactor ✅

**Status**: Complete

**Goal**: Update tisvlayd to use backend abstraction and new D-Bus interface.

### Tasks

1. Create backend registry system in tisvlayd
   - `BackendRegistry` struct for registering vendor backends
   - Discovery aggregation across all backends
   - Factory method dispatch based on vendor

2. Refactor TvManager to work with `Box<dyn TvConnection>`
   - Remove direct WebOsClient usage
   - Use trait objects for TV connections
   - Capability-aware method dispatch

3. Implement new D-Bus interface with per-TV object paths
   - `/org/tisvlay/Manager` with `Manager1` interface
   - `/org/tisvlay/Tv/{id}` with `Tv1` base interface

4. Implement feature interfaces based on TV capabilities
   - `Tv1.Power` if `POWER` capability
   - `Tv1.Screen` if `SCREEN` capability
   - `Tv1.Volume` if `VOLUME` capability
   - `Tv1.Input` if `INPUT` capability
   - `Tv1.Backlight` if `BACKLIGHT` capability

5. Update pairing flow to use signal-based state machine
   - `StartPairing()` method
   - `PairingStateChanged` signal
   - `SubmitPairingInput()` for user responses

6. Add capability-based method gating
   - Return `NotSupported` error for missing capabilities
   - Introspectable interface list based on capabilities

### Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tisvlayd/src/registry.rs` | Create |
| `crates/tisvlayd/src/dbus/mod.rs` | Create |
| `crates/tisvlayd/src/dbus/manager.rs` | Create |
| `crates/tisvlayd/src/dbus/tv.rs` | Create |
| `crates/tisvlayd/src/tv_manager.rs` | Major refactor |
| `crates/tisvlayd/src/dbus.rs` | Remove (replaced by dbus/) |
| `crates/tisvlayd/src/main.rs` | Update |
| `crates/tisvlayd/Cargo.toml` | Add tisvlay-lg dependency |

### Verification

```bash
cargo build -p tisvlayd

# Run daemon in session mode
tisvlayd --session &

# Test new D-Bus interface
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Manager
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetTvs

# Test per-TV object (if TV configured)
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/living_room

# Monitor signals
busctl --user monitor org.tisvlay.Daemon
```

### Completion Notes

- Implemented per-TV D-Bus object paths at `/org/tisvlay/Tv/{tv_id}`
- Created `TvInterface` with properties: `Id`, `Name`, `Vendor`, `Ip`, `Mac`, `Connected`, `Capabilities`
- Created `ManagerInterface` with methods: `GetTvs`, `GetSupportedVendors`, `ReloadConfig`
- Added `TvInfo` struct returned by `GetTvs` D-Bus method
- Capability-based method exposure (Power, Screen, Volume, Input)
- TvManager refactored to use `LgVendorConfig` from `tv.lg` section

---

## Phase 4: CLI & Session Service Updates ✅

**Status**: Complete

**Goal**: Update CLI and session service to work with new D-Bus interface.

### Tasks

1. Update CLI to use object paths instead of string IDs
   - Update D-Bus client to use new interface
   - Handle object path returns from Manager methods
   - Query TV objects directly for operations

2. Add vendor-awareness to CLI output
   - Show vendor in `tisvlay list` output
   - Show capabilities in `tisvlay status` output
   - Vendor-specific pairing flow

3. Update session service to query TV capabilities
   - Check capabilities before calling feature methods
   - Graceful handling of unsupported operations

4. Implement profile-based session behavior
   - Load profile for each TV
   - Merge profile settings with defaults
   - Per-TV action resolution

5. Add display-aware control logic
   - Check `only_when_active` setting
   - Query TV input before taking action
   - Skip action if TV on different input

### Files to Modify

| File | Action |
|------|--------|
| `crates/tisvlay-cli/src/main.rs` | Update |
| `crates/tisvlay-session/src/main.rs` | Update |
| `crates/tisvlay-session/src/session_monitor.rs` | Update |
| `crates/tisvlay-core/src/dbus_client.rs` | Major refactor |

### Verification

```bash
# List TVs (should show vendor)
tisvlay list

# Check capabilities
tisvlay status --tv living-room

# Test pairing flow
tisvlay pair 192.168.1.100 --name "Test TV" --vendor lg

# Test session service
tisvlay-session --user &
# Lock/unlock screen and verify TV behavior
```

### Completion Notes

- Updated `DaemonClient` with `TvProxy` for per-TV D-Bus objects
- CLI now uses `TvInfo` struct from `GetTvs()` method
- Session service implements profile-based behavior via `get_profile_for_tv()`
- Each TV responds to events according to its own profile configuration
- CLI pairing stores credentials in `[tv.lg]` section (not root level)
- Removed legacy field handling from CLI and session service

---

## Phase 4.5: Abstraction Cleanup 🔄

**Status**: In Progress

**Goal**: Eliminate leaky abstractions where LG/WebOS-specific code bleeds through the vendor-agnostic layers.

### Problem Statement

After completing Phase 4, review identified several places where the abstraction leaks:

1. **CLI direct LG imports**: The `--no-daemon` mode directly uses `tisvlay_lg::WebOsClient` instead of going through the registry/trait abstraction.

2. **Daemon vendor string checks**: Code like `if vendor == "lg"` and downcasts to `tisvlay_lg::LgConnection` bypass the trait system.

3. **Feature traits require downcasting**: Instead of `dyn TvConnection` implementing feature traits directly, the daemon downcasts to concrete types.

### Tasks

1. **Create `tisvlay-backends` composition crate**
   - New crate that imports all vendor crates (`tisvlay-lg`, future `tisvlay-samsung`, etc.)
   - Provides `BackendRegistry` with all vendors pre-registered
   - Single composition point - only this crate knows about vendor implementations
   - Extract existing `tisvlayd/src/registry.rs` logic to this crate

2. **Move protocol-specific config to vendor sections**
   - Remove `ip` and `mac` from root `TvConfig`
   - Move to `LgVendorConfig` (and future vendor configs)
   - Root `TvConfig` should only contain: `id`, `name`, `vendor`, `profile`, `hdmi_input`, `only_when_active`
   - Each vendor defines its own connection parameters (IP/MAC for network, port/baud for serial, etc.)

3. **Extend `TvConnection` trait with feature trait bounds**
   - Make `TvConnection: PowerControl + ScreenControl + VolumeControl + InputControl + BacklightControl`
   - Or use associated types / dynamic dispatch without downcasting
   - Eliminate `as_any()` / `as_any_mut()` pattern if possible

4. **Update daemon to use `tisvlay-backends`**
   - Remove direct `tisvlay-lg` import from `tisvlayd`
   - Import `tisvlay-backends` instead
   - Use `tisvlay_backends::create_registry()` to get pre-populated registry
   - Daemon becomes vendor-agnostic

5. **Update CLI to use `tisvlay-backends`**
   - Remove direct `tisvlay-lg` import from CLI
   - Import `tisvlay-backends` for `--no-daemon` mode
   - CLI becomes vendor-agnostic

6. **Eliminate vendor string checks and downcasts**
   - Replace `if vendor == "lg"` with polymorphic dispatch
   - Remove `downcast_mut::<tisvlay_lg::LgConnection>()` calls
   - Feature traits accessible via `dyn TvConnection` directly

7. **Update BackendRegistry interface**
   - `connect()` should return `Box<dyn TvConnection>` with all capabilities
   - Callers query capabilities via `Capabilities` bitflags, not downcasting

### Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tisvlay-backends/` | **CREATE** - New composition crate |
| `crates/tisvlay-backends/Cargo.toml` | Import `tisvlay-core`, `tisvlay-lg` |
| `crates/tisvlay-backends/src/lib.rs` | Registry with all vendors registered |
| `crates/tisvlay-core/src/config.rs` | Move `ip`, `mac` from `TvConfig` to `LgVendorConfig` |
| `crates/tisvlay-core/src/traits.rs` | Update trait bounds |
| `crates/tisvlay-lg/src/connection.rs` | Implement updated traits |
| `crates/tisvlay-cli/src/main.rs` | Replace `tisvlay_lg` → `tisvlay_backends` |
| `crates/tisvlay-cli/Cargo.toml` | Replace `tisvlay-lg` → `tisvlay-backends` |
| `crates/tisvlayd/src/tv_manager.rs` | Remove downcasts, use registry from backends |
| `crates/tisvlayd/src/registry.rs` | Remove (moved to tisvlay-backends) |
| `crates/tisvlayd/Cargo.toml` | Replace `tisvlay-lg` → `tisvlay-backends` |
| `Cargo.toml` (workspace) | Add `tisvlay-backends` member |

### Verification

```bash
# Ensure CLI only imports tisvlay-backends, not vendor crates directly
grep -r "tisvlay_lg" crates/tisvlay-cli/   # Should return nothing
grep -r "tisvlay_backends" crates/tisvlay-cli/  # Should find imports

# Ensure daemon only imports tisvlay-backends, not vendor crates directly
grep -r "tisvlay_lg" crates/tisvlayd/   # Should return nothing
grep -r "tisvlay_backends" crates/tisvlayd/  # Should find imports

# Ensure no vendor string comparisons in daemon/CLI
grep -r "vendor == " crates/tisvlayd/ crates/tisvlay-cli/

# Ensure no downcasts to vendor-specific types
grep -r "downcast.*Lg" crates/tisvlayd/ crates/tisvlay-cli/

# Only tisvlay-backends should import vendor crates
grep -r "tisvlay-lg" crates/tisvlay-backends/Cargo.toml  # Should find it here
grep -r "tisvlay-lg" crates/tisvlayd/Cargo.toml  # Should NOT find it
grep -r "tisvlay-lg" crates/tisvlay-cli/Cargo.toml  # Should NOT find it

# All tests pass
cargo test --workspace
```

### Design Decisions

**`tisvlay-backends` as composition crate**: To avoid circular dependencies and keep binaries vendor-agnostic:

```
tisvlay-core (traits, types, D-Bus client)
      ↑
tisvlay-lg, tisvlay-samsung, ... (vendor implementations)
      ↑
tisvlay-backends (registry - ONLY crate that knows about vendors)
      ↑
tisvlayd, tisvlay-cli, tisvlay-session (vendor-agnostic binaries)
```

**Adding a new vendor requires only**:
1. Create `tisvlay-newvendor` crate implementing core traits
2. Add dependency to `tisvlay-backends/Cargo.toml`
3. Register backend in `tisvlay-backends/src/lib.rs`

**No changes needed** to daemon, CLI, GUI, or session service.

**IP/MAC moved to vendor-specific config**: Protocol-specific connection parameters belong in vendor sections, not root config:
- Root `TvConfig` contains only universal fields: `id`, `name`, `vendor`, `profile`, `hdmi_input`, `only_when_active`
- `LgVendorConfig` contains: `ip`, `mac`, `client_key`, `default_input`
- Future vendors define their own connection parameters (e.g., serial port, cloud API token, etc.)
- This ensures the core abstraction doesn't assume network-based communication

**New config structure**:
```toml
[[tv]]
id = "living-room"
name = "Living Room TV"
vendor = "lg"
profile = "default"

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123..."
default_input = "HDMI_1"
```

**What MUST change**:
- Create `tisvlay-backends` crate as single composition point
- `ip` and `mac` removed from root `TvConfig`
- CLI and daemon import `tisvlay-backends`, not vendor crates directly
- No `if vendor == "lg"` checks or downcasts in daemon/CLI

### Completion Notes (Partial)

**Completed tasks:**

1. ✅ **Created `tisvlay-backends` composition crate**
   - New crate at `crates/tisvlay-backends/`
   - Contains `BackendRegistry`, `RegisteredBackend`, and `create_registry()` function
   - Re-exports common types from `tisvlay-core` for convenience
   - Only crate that directly imports vendor crates (`tisvlay-lg`)

2. ✅ **Moved IP/MAC to vendor-specific config**
   - Removed `ip` and `mac` from root `TvConfig`
   - Added `ip: String` and `mac: Option<String>` to `LgVendorConfig`
   - Updated all config parsing tests
   - Updated all usages in daemon (`tv_manager.rs`) and CLI (`main.rs`)

3. ✅ **Updated daemon to use `tisvlay-backends`**
   - Daemon's `registry.rs` now re-exports from `tisvlay-backends`
   - `create_default_registry()` calls `tisvlay_backends::create_registry()`
   - Daemon still imports `tisvlay-lg` for `LgConfig` type (needed until task 6)

**Remaining tasks:**

4. ⏳ **Extend TvConnection trait with feature trait bounds** - Deferred
5. ⏳ **Update CLI to use `tisvlay-backends`** - CLI still uses `tisvlay_lg::WebOsClient` directly for `--no-daemon` mode
6. ⏳ **Eliminate vendor string checks and downcasts** - Daemon still has downcasts to `LgConnection`

**Note**: Tasks 4-6 require significant refactoring of the trait system to eliminate downcasting. This is tracked as future work since the current implementation functions correctly.

---

## Phase 5: Testing & Documentation ⏳

**Status**: Pending (blocked by Phase 4.5)

**Goal**: Ensure everything works and document the architecture.

### Tasks

1. Update unit tests for new trait-based architecture
   - Mock implementations for testing
   - Capability-based test cases

2. Add integration tests for LG backend
   - Mock WebSocket server
   - Protocol compliance tests

3. Update CLAUDE.md with new architecture
   - Update crate structure
   - Update implementation status

4. Update docs/design.md with multi-vendor design
   - Link to multi-vendor-design.md
   - Update architecture diagrams

5. Create docs/extending.md for adding new backends
   - Step-by-step guide
   - Required trait implementations
   - Discovery and pairing requirements

### Files to Create/Modify

| File | Action |
|------|--------|
| `crates/tisvlay-core/src/tests/` | Create/update |
| `crates/tisvlay-lg/src/tests/` | Create |
| `CLAUDE.md` | Update |
| `docs/design.md` | Update |
| `docs/extending.md` | Create |

### Verification

```bash
# Run all tests
cargo test --workspace

# Run with coverage (if available)
cargo tarpaulin --workspace

# Build documentation
cargo doc --workspace --no-deps
```

---

## Dependency Order

```
Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4 ──► Phase 4.5
   ✅          ✅          ✅          ✅          🔄
                                                  │
                                                  ▼
                                              Phase 5
                                                 ⏳
```

- **Phase 1** must complete before Phase 2 (traits needed for backend impl) ✅
- **Phase 2** must complete before Phase 3 (backend needed for daemon) ✅
- **Phase 3** must complete before Phase 4 (new D-Bus interface needed) ✅
- **Phase 4** must complete before Phase 4.5 (need working system to refactor) ✅
- **Phase 4.5** partially complete (tisvlay-backends crate created, ip/mac moved) 🔄
- **Phase 5** can start once Phase 4.5 core tasks complete ⏳

---

## Estimated Scope

| Phase | New Files | Modified Files | Complexity | Status |
|-------|-----------|----------------|------------|--------|
| 1 | 3 | 3 | Medium | ✅ Complete |
| 2 | 8 | 3 | High | ✅ Complete |
| 3 | 4 | 3 | High | ✅ Complete |
| 4 | 0 | 4 | Medium | ✅ Complete |
| 4.5 | 3 | 9 | Medium | 🔄 In Progress |
| 5 | 2 | 3 | Low | ⏳ Pending |
| **Total** | **20** | **25** | | |

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Trait design doesn't fit all backends | Design with LG in mind, but review against Samsung/Sony APIs |
| D-Bus interface too complex | Start with minimal interfaces, add features incrementally |
| Breaking changes cause regressions | Comprehensive test coverage before major refactors |
| Config migration painful | Provide clear migration docs; consider migration script |
| Leaky abstractions | Phase 4.5 specifically addresses this; verify with grep checks |
