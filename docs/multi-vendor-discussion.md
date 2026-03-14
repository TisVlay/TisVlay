# Multi-Vendor Refactor Discussion

This document captures the design discussion and decision-making process for the tisvlay multi-vendor refactor. It serves as context for why certain architectural decisions were made.

> **Implementation Status**: Phases 1-4 complete; Phase 4.5 (abstraction cleanup) pending. See [multi-vendor-implementation-plan.md](multi-vendor-implementation-plan.md) for details.

---

## Background

After rebranding the project from `orlgtv` to `tisvlay`, the question arose: should we abstract the TV-specific interfaces to allow for future support of other TV brands beyond LG WebOS?

### Initial Assessment

An exploration of the codebase revealed **tight coupling to LG WebOS (8/10)**:

| Component | Coupling Level | Key Issue |
|-----------|----------------|-----------|
| `protocol.rs` | CRITICAL | All message types are SSAP-specific with hardcoded signature |
| `endpoints.rs` | CRITICAL | 31+ hardcoded `ssap://` URIs |
| `client.rs` | CRITICAL | `WebOsClient` is a concrete impl with no trait abstraction |
| `connection.rs` | HIGH | Ports 3000/3001 hardcoded, LG cert handling |
| `discovery.rs` | HIGH | LG-specific SSDP filtering |
| `config.rs` | HIGH | LG-specific fields (`client_key`, `mac`) |
| `wol.rs` | LOW | Standard WoL (already generic) |

**What's already generic**: WoL implementation, TOML config loading, D-Bus interface (though it calls LG-specific code underneath).

---

## Design Questions & Decisions

### Q1: Which other TV brands to support?

**Options considered:**
- Samsung (Tizen) - Uses proprietary SmartThings/Tizen API
- Sony (Android TV) - Uses BRAVIA Professional Display API over HTTP REST
- Roku TV (TCL, Hisense) - Uses External Control Protocol (ECP) over HTTP
- Keep it open-ended

**Decision**: **Keep it open-ended** - Design for unknown future vendors without targeting specific ones now. This avoids over-engineering for hypothetical requirements while ensuring the architecture is extensible.

---

### Q2: How should vendor-specific modules be organized?

**Options considered:**
1. Separate crates (`tisvlay-lg`, `tisvlay-samsung`, etc.)
2. Feature flags in core crate
3. Runtime plugin system

**Decision**: **Separate crates** - Cleaner boundaries, optional compilation, easier to manage source files. Each vendor implementation is self-contained while sharing core abstractions.

---

### Q3: How should vendor-specific features be exposed?

**Options considered:**
1. Capability enum - TVs report capabilities, CLI/GUI adapt dynamically
2. Trait per feature - Separate traits for PowerControl, ScreenControl, etc.
3. Unified trait with Option returns

**Decision**: **Capability enum** combined with feature traits. TVs advertise capabilities via bitflags, and the daemon exposes D-Bus interfaces based on what the TV supports. This allows:
- Runtime capability detection
- Dynamic UI adaptation
- Graceful handling of unsupported operations

---

### Q4: What's the priority for refactor scope?

**Options considered:**
1. Minimal viable abstraction - Just enough to add one more vendor later
2. Full trait-based architecture - Complete refactor with clean abstractions
3. Focus on D-Bus/CLI stability

**Decision**: **Full trait-based architecture** - Since the project is still in prototyping stage with no stability commitments, this is the ideal time for a comprehensive refactor. Better to get the abstractions right now than retrofit later.

---

### Q5: How should the D-Bus interface evolve?

**Options considered:**

**Approach A: Keep current flat API**
- Add 'vendor' field to TV info
- Methods stay the same, return errors for unsupported ops

**Approach B: Per-TV object paths with properties**
```
/org/tisvlay/Manager              # Manager interface
/org/tisvlay/Tv/{tv_id}           # Per-TV objects
```
- Each TV gets its own object path
- D-Bus properties for capabilities
- PropertiesChanged signals

**Approach C: Feature-based sub-interfaces**
- Base `org.tisvlay.Tv1` interface for all TVs
- Optional `org.tisvlay.Tv1.ScreenControl`, `org.tisvlay.Tv1.VolumeControl`, etc.

**Decision**: **Hybrid of B and C** - This is the most idiomatic D-Bus design, following patterns used by NetworkManager, UDisks2, and systemd:
- Manager at `/org/tisvlay/Manager` for discovery and TV lifecycle
- Per-TV objects at `/org/tisvlay/Tv/{id}` with base `Tv1` interface
- Feature interfaces (`Tv1.Power`, `Tv1.Screen`, etc.) implemented based on capabilities
- Standard PropertiesChanged signals for state updates

Backwards compatibility was explicitly **not a concern** since the project is in prototyping stage.

---

### Q6: How should multi-vendor discovery work?

**Options considered:**
1. Unified discovery - Single `DiscoverTvs` call returns all vendors
2. Per-vendor discovery - `DiscoverLgTvs`, `DiscoverSamsungTvs`, etc.
3. Pluggable discovery - Modules register with daemon

**Decision**: **Unified discovery** - Single call returns all discovered TVs, categorized by vendor. Filtering done server-side. This simplifies the client API while maintaining flexibility.

---

### Q7: How should configuration handle vendor-specific settings?

**Options considered:**
1. Nested vendor section - `[[tv]]` with `[tv.lg]` subsection
2. Flat with prefixes - `lg_client_key`, `samsung_token`, etc.
3. Separate config files

**Decision**: **Nested vendor section** - Common fields at top level, vendor-specific in nested sections:

```toml
[[tv]]
id = "living-room"
name = "Living Room G4"
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
vendor = "lg"

[tv.lg]
client_key = "abc123..."
default_input = "HDMI_1"
```

This keeps everything in a single config file while maintaining clear separation.

---

### Q8: Should refactor happen before or after GUI/tray work?

**Decision**: **Before** - Stabilize core abstractions first so GUI/tray are built on a stable foundation. This prevents having to refactor the GUI layer later when the underlying abstractions change.

---

## Multi-TV Use Case Discussion

### User Scenarios Identified

1. **Multi-monitor desk setups** - Multiple TVs connected to same PC, all should behave the same (e.g., all blank when screen locks)

2. **Multi-room setups** - Different TVs in different rooms (office vs living room), behavior depends on which display is currently active

3. **Display-aware control** - Don't change TV state if the TV is currently on a different input (e.g., someone watching a movie via a streaming device)

### Profile System Design

Instead of per-TV overrides, a **profile/group system** was chosen:

```toml
# Default profile (fallback)
[profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"

# Desk profile - all office monitors
[profiles.desk]
idle_timeout_seconds = 300
on_idle_timeout = "screen_off"

# Gaming profile - don't interrupt
[profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[[tv]]
id = "office-left"
profile = "desk"
hdmi_input = "HDMI_1"
only_when_active = true
```

The `only_when_active` + `hdmi_input` combo handles the "don't interrupt movie watching" use case.

---

## Pairing Architecture Discussion

### Challenge

Different TV vendors have different pairing mechanisms:
- **LG WebOS**: Interactive prompt on TV screen, user must accept
- **Other vendors**: May require PIN entry, password, OAuth, etc.

### GUI Setup Wizard UX Vision

The pairing wizard UX should be **similar to adding a printer** in a typical desktop environment:

1. **Discovery Screen**: User is greeted with a list of discovered TV devices, categorized by vendor. Devices could be discovered from:
   - Scanning the local LAN network (SSDP, mDNS, etc.)
   - Plugged-in serial/USB ports (future consideration)

2. **Manual Add Option**: If the device is not discovered, the user can manually add one by:
   - Selecting from supported vendor options
   - Filling out vendor/device-specific connection information (IP, port, etc.)

3. **Adaptive Pairing Flow**: The pairing UX adapts based on the device vendor/type selected:
   - **LG WebOS**: "Please accept the pairing prompt on your TV screen..."
   - **PIN-based vendors**: "Enter the PIN code displayed on your TV"
   - **Password-based vendors**: "Enter the password you configured on your TV"

This printer-like UX metaphor helps users understand the flow intuitively, as most users have experience adding printers to their systems.

### Solution: Signal-Based State Machine

For the internal Rust implementation:
```rust
pub enum PairingState {
    AwaitingTvPrompt,                    // LG: "Accept on TV screen"
    AwaitingUserInput { prompt: String, input_type: InputType },
    Complete { credentials: Credentials },
    Failed { error: PairingError },
}

pub enum InputType {
    None,      // Just waiting
    Pin,       // Enter PIN shown on TV
    Password,  // Enter pre-configured password
}
```

For D-Bus, this maps to signals:
```xml
<signal name="PairingStateChanged">
  <arg name="tv" type="o"/>
  <arg name="state" type="s"/>      <!-- "awaiting_tv", "awaiting_input", "complete", "failed" -->
  <arg name="message" type="s"/>    <!-- Human-readable prompt -->
  <arg name="input_type" type="s"/> <!-- "none", "pin", "password" -->
</signal>
```

The GUI wizard subscribes to `PairingStateChanged` and adapts its UI based on the state and `input_type`.

---

## Connection Types Discussion

### Question: Should non-network TV control be in scope?

Some commercial displays use RS-232 or USB for control, not just network protocols.

**Decision**: **Abstract for future** - Design the connection abstraction to support non-network transports, but only implement network (IP-based) protocols initially. This future-proofs the architecture without adding unnecessary complexity now.

---

## Terminology Decision

### "Brand" vs "Vendor"

Initially the design used "brand" (e.g., `brand = "lg"`), but **"vendor"** was chosen as more technically precise and commonly used in hardware/software contexts.

All references updated:
- `brand_id()` → `vendor_id()`
- `TvConfig.brand` → `TvConfig.vendor`
- D-Bus property `Brand` → `Vendor`
- `GetSupportedBrands` → `GetSupportedVendors`

---

## Key Architectural Decisions Summary

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Module organization | Separate crates | Clean boundaries, optional compilation |
| Feature exposure | Capability bitflags + feature traits | Runtime detection, dynamic UI |
| D-Bus design | Per-TV objects + feature interfaces | Idiomatic D-Bus, follows established patterns |
| Config format | Nested vendor sections + profiles | Single file, clear separation |
| Discovery | Unified across vendors | Simple client API |
| Pairing | Signal-based state machine | Handles diverse auth mechanisms |
| Refactor timing | Before GUI/tray work | Stable foundation first |
| Terminology | "Vendor" not "brand" | Technical precision |
| IP/MAC location | Vendor-specific config | Root config should be transport-agnostic |
| Leaky abstractions | Phase 4.5 cleanup | CLI/daemon must not import vendor crates directly |
| Vendor composition | `tisvlay-backends` crate | Single composition point; binaries remain vendor-agnostic |

---

---

## Implementation Notes

These notes capture decisions and adjustments made during actual implementation (Phases 1-4).

### LgVendorConfig Location

**Decision during implementation**: `LgVendorConfig` is defined in `tisvlay-core/src/config.rs` rather than `tisvlay-lg/src/config.rs`.

**Rationale**: Keeping all config structs together simplifies serde handling and avoids circular dependencies between crates. The vendor-specific config structs are small data containers, not complex implementations.

### Legacy Field Removal

**Decision during implementation**: No backwards compatibility for legacy `client_key` and `default_input` fields at the TV root level.

**Rationale**: Project is in prototyping stage. Clean break is better than carrying compatibility shims. All LG-specific config now lives exclusively under `[tv.lg]` section.

### Trait Implementation Deferred

**Decision during implementation**: Full trait-based abstraction (`TvBackend`, `TvConnection`, feature traits) defined but not fully implemented for LG backend yet.

**Rationale**: The `WebOsClient` continues to work directly. Trait implementations can be added when a second vendor is actually being implemented, avoiding premature abstraction.

### D-Bus TvInfo Struct

**Decision during implementation**: Added `TvInfo` struct returned by `GetTvs()` method containing: `id`, `name`, `vendor`, `ip`, `mac`, `connected`, `capabilities`.

**Rationale**: More type-safe than returning tuple of strings. CLI and other clients can work with structured data.

---

## Post-Implementation Review: Leaky Abstractions

After completing Phase 4, a review identified that the abstraction was "leaking" in several places:

### Issues Identified

1. **CLI direct LG imports**: The `--no-daemon` mode directly imports and uses `tisvlay_lg::WebOsClient`, bypassing the trait abstraction entirely.

2. **Daemon vendor string checks**: Code patterns like `if vendor == "lg"` and downcasts to `tisvlay_lg::LgConnection` were present in `tv_manager.rs`.

3. **Feature traits require downcasting**: Instead of `dyn TvConnection` implementing feature traits directly, the daemon was downcasting to concrete types via `as_any_mut().downcast_mut::<LgConnection>()`.

### Q9: Should IP/MAC be moved to vendor-specific config?

**Options considered:**
1. Move `ip` and `mac` from root `TvConfig` to vendor-specific sections
2. Keep them at root level as "common network fields"
3. Add a `connection_type` enum to distinguish network vs serial vs USB

**Initial decision** (later revised): Keep IP/MAC at root level since most protocols are network-based.

**Revised decision**: **Move IP/MAC to vendor-specific config** - The root `TvConfig` should be truly vendor-agnostic:
- Not all vendors use network-based protocols (serial RS-232, USB, cloud APIs with device IDs)
- Not all vendors support Wake-on-LAN
- Having `ip` and `mac` at root level assumes network connectivity, which is a protocol-specific detail
- Clean separation: root config has identity/behavior fields, vendor config has connection parameters

**New structure**:
```toml
[[tv]]
id = "living-room"
name = "Living Room TV"
vendor = "lg"
profile = "default"

[tv.lg]
ip = "192.168.1.100"          # LG-specific: WebSocket connection
mac = "AA:BB:CC:DD:EE:FF"     # LG-specific: Wake-on-LAN
client_key = "abc123..."
default_input = "HDMI_1"

# Future example: serial-connected display
# [tv.serial]
# port = "/dev/ttyUSB0"
# baud_rate = 9600
```

This ensures the core abstraction makes no assumptions about the underlying transport.

### Q10: How should leaky abstractions be addressed?

**Decision**: Add **Phase 4.5** specifically to clean up leaky abstractions before documentation/testing in Phase 5.

**Key changes for Phase 4.5:**
- CLI should never import `tisvlay_lg` directly; use registry pattern
- Daemon should not have vendor string checks; use polymorphic dispatch
- No downcasting to vendor-specific connection types
- Feature traits should be accessible via `dyn TvConnection`

This ensures adding a second vendor (e.g., Samsung) doesn't require touching CLI or daemon code—only adding a new crate and registering the backend.

### Q11: Where should vendor crates be composed?

**Problem**: If `tisvlay-core` imported vendor crates, it would create circular dependencies:
```
tisvlay-core → tisvlay-lg → tisvlay-core  ❌ CIRCULAR
```

**Options considered:**
1. Daemon as composition root (each binary imports vendor crates)
2. Create a `tisvlay-backends` crate as single composition point
3. Remove `--no-daemon` mode (CLI only works via D-Bus)
4. Use feature flags to optionally include vendors

**Decision**: **Create `tisvlay-backends` composition crate**

```
tisvlay-core (traits, types, D-Bus client)
      ↑
tisvlay-lg, tisvlay-samsung, ... (vendor implementations)
      ↑
tisvlay-backends (registry - ONLY crate that knows vendors)
      ↑
tisvlayd, tisvlay-cli, tisvlay-gui, tisvlay-session
```

**Rationale**:
- Single point of vendor registration - only `tisvlay-backends` needs updating for new vendors
- All binaries (daemon, CLI, GUI, session) remain vendor-agnostic
- No circular dependencies
- Clean separation of concerns

**Adding a new vendor**:
1. Create `tisvlay-newvendor` crate implementing core traits
2. Add to `tisvlay-backends/Cargo.toml`
3. Register in `tisvlay-backends/src/lib.rs`

**No changes** to daemon, CLI, GUI, or session service code.

---

## References

- [multi-vendor-design.md](multi-vendor-design.md) - Full architecture specification
- [multi-vendor-implementation-plan.md](multi-vendor-implementation-plan.md) - Phased implementation plan
- [design.md](design.md) - Original LG-specific design document
- [reference.md](reference.md) - LG WebOS protocol documentation
