# tisvlay Multi-Vendor Refactor Implementation Plan

This document outlines the phased implementation plan for refactoring tisvlay to support multiple TV vendors.

See [multi-vendor-design.md](multi-vendor-design.md) for the full architecture and design specification.

---

## Phase 1: Core Abstractions (Foundation)

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

---

## Phase 2: LG Backend Extraction

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

---

## Phase 3: Daemon Refactor

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

---

## Phase 4: CLI & Session Service Updates

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

---

## Phase 5: Testing & Documentation

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
Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4
                                      │
                                      ▼
                                  Phase 5
```

- **Phase 1** must complete before Phase 2 (traits needed for backend impl)
- **Phase 2** must complete before Phase 3 (backend needed for daemon)
- **Phase 3** must complete before Phase 4 (new D-Bus interface needed)
- **Phase 5** can run partially in parallel with Phase 4

---

## Estimated Scope

| Phase | New Files | Modified Files | Complexity |
|-------|-----------|----------------|------------|
| 1 | 3 | 3 | Medium |
| 2 | 8 | 3 | High |
| 3 | 4 | 3 | High |
| 4 | 0 | 4 | Medium |
| 5 | 2 | 3 | Low |
| **Total** | **17** | **16** | |

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Trait design doesn't fit all backends | Design with LG in mind, but review against Samsung/Sony APIs |
| D-Bus interface too complex | Start with minimal interfaces, add features incrementally |
| Breaking changes cause regressions | Comprehensive test coverage before major refactors |
| Config migration painful | Provide clear migration docs; consider migration script |
