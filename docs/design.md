# tisvlay Design Document

## Overview

**tisvlay** is a Rust utility for controlling smart TVs from a Linux desktop connected via HDMI. It provides power management, HDMI input switching, screen blanking (OLED protection), and volume control through a vendor-agnostic architecture with pluggable backends (currently LG WebOS via SSAP over WebSocket).

---

## Architecture

```mermaid
graph TD
    subgraph User Space
        CLI["CLI<br/><i>tisvlay</i>"]
        Tray["Tray<br/><i>tisvlay-tray (ksni)</i>"]
        GUI["GUI<br/><i>tisvlay-gui (Slint)</i>"]
        Session["User Session Service<br/><i>D-Bus session events: lock/unlock</i>"]
    end

    subgraph System Space
        Daemon["tisvlayd (System Daemon)<br/><i>Vendor-agnostic TV management<br/>Wake-on-LAN at boot<br/>Per-TV D-Bus objects</i>"]
        LG["LG TV<br/><i>WebSocket 3000/3001</i>"]
        Samsung["Samsung<br/><i>(TBD)</i>"]
        Other["..."]
    end

    CLI -->|D-Bus| Daemon
    Tray -->|D-Bus| Daemon
    GUI -->|D-Bus| Daemon
    Session -->|D-Bus| Daemon
    Daemon --> LG
    Daemon --> Samsung
    Daemon --> Other
```

### Components

| Component | Binary | Description |
|-----------|--------|-------------|
| **System Daemon** | `tisvlayd` | Systemd system service. Vendor-agnostic TV management, D-Bus API, WoL at boot. |
| **User Service** | `tisvlay-session` | Systemd user service. Monitors D-Bus session events (session lock/unlock, idle); reactively prunes orphan user-config `[[tv]]` assignments via `ObjectManager.InterfacesRemoved`. |
| **CLI** | `tisvlay` | Command-line interface. Communicates with daemon via D-Bus. |
| **Tray App** | `tisvlay-tray` | System tray icon using `ksni`. Subscribes to daemon D-Bus signals for real-time state updates (30s fallback poll). Per-TV power/screen/volume/backlight/input controls with scroll wheel support. |
| **GUI** | `tisvlay-gui` | Slint-based configuration window. TVs pane with master/detail layout, per-TV editor (live state controls + edit-mode pencil for config edits), pairing wizard with discovery streaming. Profiles pane with two-section master list (System / User) + per-profile detail editor (rename / duplicate / delete + per-field inherit-or-override rows). About pane built; Session pane still a placeholder pending M5 in [docs/plan.md](plan.md). |

### Crate Structure

See `CLAUDE.md` for the full file tree. Key crates:

| Crate | Purpose |
|-------|---------|
| `tisvlay-core` | Core traits (`TvBackend`, `TvConnection`, `TvConnector`), types, config, schema (`PairingField`, `FieldVisibility`), D-Bus client |
| `tisvlay-lg` | LG WebOS backend (WebSocket, SSAP protocol, SSDP discovery) |
| `tisvlay-mock` | In-memory mock backend (dev/tests, optional via `mock` feature) |
| `tisvlay-test` | OS-environment fakes — private `dbus-daemon`, fake logind, fake screensaver |
| `tisvlay-backends` | Composition crate — only place that imports vendor crates |
| `tisvlay-harness` | Binary-lifecycle test fixtures (`TestDaemon`, `TestSession`, `TestTray`) |
| `tisvlayd` | System daemon — vendor-agnostic TV management |
| `tisvlay-cli` | CLI tool — vendor-agnostic |
| `tisvlay-session` | User session service — vendor-agnostic |
| `tisvlay-tray` | System-tray app (ksni) — vendor-agnostic |
| `tisvlay-gui` | Slint-based settings window — vendor-agnostic; status in [plan.md](plan.md) |

### Dependency Graph

```mermaid
graph BT
    core["tisvlay-core<br/><i>traits, types, schema, D-Bus client</i>"]
    lg["tisvlay-lg"]
    mock["tisvlay-mock"]
    samsung["tisvlay-samsung, ..."]
    backends["tisvlay-backends<br/><i>composition — ONLY crate that imports vendors</i>"]
    daemon["tisvlayd<br/><i>system daemon</i>"]
    cli["tisvlay-cli"]
    session["tisvlay-session"]
    tray["tisvlay-tray"]
    gui["tisvlay-gui"]

    lg --> core
    mock --> core
    samsung -.-> core
    backends --> lg
    backends -->|"mock feature"| mock
    backends -.-> samsung
    daemon --> backends
    cli -->|"dbus feature"| core
    session -->|"dbus feature"| core
    tray -->|"dbus feature"| core
    gui -->|"dbus feature"| core
```

**Key principle**: Only `tisvlay-backends` knows about vendor crates. All binaries are vendor-agnostic. No `if vendor == "lg"` checks or downcasts in any binary code.

Adding a new vendor requires only:
1. Create `tisvlay-newvendor` crate implementing core traits
2. Add dependency to `tisvlay-backends/Cargo.toml`
3. Register backend in `tisvlay-backends/src/lib.rs`

No changes needed to daemon, CLI, or session service.

---

## Core Traits

### Trait Hierarchy

```mermaid
classDiagram
    class TvBackend {
        +vendor_id() &str
        +vendor_name() &str
        +capabilities() Capabilities
    }

    class TvConnector {
        +transport() Transport
        +connect(vendor_config) TvConnection
        +config_schema() Vec~PairingField~
        +pairing_schema() Vec~PairingField~
        +display_config(vendor_config) Vec~(String,String)~
        +display_diagnostics(vendor_config) Vec~(String,String)~
        +supports_wake(vendor_config) bool
        +wake(vendor_config)
        +wake_and_connect(vendor_config, ...)
        +start_pairing(vendor_config) PairingSession
        +build_config_from_pairing(...)
    }

    class TvDiscovery {
        +discover(timeout) Vec~DiscoveredTv~
    }

    class TvConnection {
        +vendor_id() &str
        +is_connected() bool
        +get_state() TvState
        +close()
        +capabilities() Capabilities
        +power_off()
        +get_power_state() PowerState
        +screen_on()
        +screen_off()
        +is_screen_on() bool
        +set_volume(level: u8)
        +get_volume() u8
        +set_mute(mute: bool)
        +is_muted() bool
        +switch_input(input_id: &str)
        +get_current_input() Option~String~
        +list_inputs() Vec~InputInfo~
        +set_backlight(level: u8)
        +get_backlight() u8
    }

    TvBackend <|-- TvConnector : implements alongside
    TvBackend <|-- TvDiscovery : implements alongside
    TvConnector ..> TvConnection : creates
```

See `crates/tisvlay-core/src/traits.rs` for the full trait definitions.

### Transport Agnosticism

The `TvConnector` interface is fully transport-agnostic. All transport details (IP addresses, serial ports, CEC addresses, etc.) are encapsulated in the vendor config (`toml::Value`), which only the vendor crate knows how to interpret. The daemon and CLI never see transport-specific types.

Each connector declares its `Transport` type (`Network`, `Serial`, `Usb`, `Cec`). The daemon uses `Transport::is_network_dependent()` to scope system events — only network-dependent connections are affected by NetworkManager events. Non-network backends (serial, USB, CEC) are unaffected by network loss.

Key `TvConnector` methods:
- `transport()` — communication transport used by this connector
- `connect(vendor_config)` — create a connection using vendor-specific transport
- `config_schema()` — declarative `Vec<PairingField>` describing the persisted-config shape with `FieldVisibility` (`UserInput` / `Diagnostic` / `Secret`) per field. Drives the pairing wizard, the GUI's edit pane, and the daemon's `Tv1.{VendorConfig,VendorInfo}` projections — backends declare schema once and the framework projects per-use-case.
- `display_config(vendor_config)` / `display_diagnostics(vendor_config)` — `(label, value)` projection of `UserInput` / `Diagnostic` fields respectively, default-implemented via `config_schema`. `Secret` fields are excluded from both.
- `supports_wake(vendor_config)` / `wake(vendor_config)` — abstract over WoL, CEC, etc.
- `start_pairing(vendor_config)` / `build_config_from_pairing(...)` — vendor-specific pairing

### TvConnection

`TvConnection` has all feature methods directly with default implementations returning `Error::NotSupported`. Backends only override the methods they support. Callers use `capabilities()` bitflags to check availability before calling.

Note: `power_on()` is not a `TvConnection` method — power-on is handled at the daemon level via `TvConnector::wake()` (WoL, CEC, etc.) since it requires a new connection to be established.

### Capabilities

Bitflags in `crates/tisvlay-core/src/capabilities.rs`: `POWER`, `SCREEN`, `VOLUME`, `MUTE`, `INPUT`, `BACKLIGHT`, `WOL`, `DISCOVERY`, `ALL`.

---

## D-Bus Interface

See `crates/tisvlayd/src/dbus/` for the implementation.

### Service overview

```
org.tisvlay.Daemon  (system bus; session bus in dev)
├── /org/tisvlay                    ObjectManager
├── /org/tisvlay/Manager            org.tisvlay.Manager1
│   ├── Methods:  GetSupportedVendors, GetPairingSchema, AddTv,
│   │             CancelPair, ReloadConfig, CreateSystemProfile,
│   │             DeleteSystemProfile, DuplicateSystemProfile, DiscoverTvs
│   └── Signals:  ConfigReloaded, PairingProgress, DiscoveryFound
├── /org/tisvlay/Tv/{uuid_underscored}   org.tisvlay.Tv1, Tv1.Power, Tv1.Screen,
│   │                                    Tv1.Volume, Tv1.Backlight, Tv1.Input
│   ├── Properties (rw): Name, Managed, SystemProfile, PrimaryInput,
│   │                     IsScreenOn, Volume, Muted, Backlight
│   ├── Properties (ro): Uuid, Vendor, Connected, Capabilities,
│   │                     VendorInfo, AutomateWhen, Automated,
│   │                     PowerState, CurrentInput, AvailableInputs
│   ├── Methods:  Reconnect, RePair, Remove, GetConfig, UpdateConfig,
│   │             PowerOn, PowerOff, SwitchInput
│   └── Signals:  (standard PropertiesChanged only)
└── /org/tisvlay/SystemProfile/{uuid_underscored}   org.tisvlay.SystemProfile1
    ├── Properties (rw): Name, OnStartup, OnShutdown, OnReboot,
    │                     OnNoSession, OnSleep, OnWake, AutomateWhen
    ├── Properties (ro): Id, LocallySetFields, ReferencedBy
    └── Methods:  ResetField, ResetAll

org.tisvlay.Session  (session bus)
├── /org/tisvlay                    ObjectManager
├── /org/tisvlay/Session            org.tisvlay.Session1
│   ├── Methods:  CreateUserProfile, DeleteUserProfile,
│   │             DuplicateUserProfile, GetTvUserProfile,
│   │             SetTvUserProfile, ReloadConfig
│   └── Signals:  UserProfileChanged
└── /org/tisvlay/UserProfile/{uuid_underscored}     org.tisvlay.UserProfile1
    ├── Properties (rw): Name, OnSessionLock, OnSessionUnlock,
    │                     OnGreeterWake, OnIdleTimeout, OnIdleResume,
    │                     IdleTimeoutSeconds
    ├── Properties (ro): Id, LocallySetFields, ReferencedBy
    └── Methods:  ResetField, ResetAll
```

### Clients and signal flow

| Client | Calls | Subscribes to |
|--------|-------|---------------|
| **tisvlay CLI** | Manager1 (profiles, config), Tv1.* (properties + commands), Session1 (user profiles) | — |
| **tisvlay-tray** | Manager1 (profiles), Tv1.* (properties + commands), Session1 (user profiles) | `PropertiesChanged` on Tv paths; `InterfacesAdded`/`InterfacesRemoved` (ObjectManager); Manager1: `ConfigReloaded`; Session1: `UserProfileChanged`; DBus: `NameOwnerChanged` (service discovery) |
| **tisvlay-session** | Manager1 (admin defaults), Tv1.* (session actions via writable properties) | — |

`org.freedesktop.DBus.ObjectManager` at `/org/tisvlay` provides `GetManagedObjects()` — a single round-trip returning all TV object paths and their interface properties.

### org.tisvlay.Manager1

Methods:
- `GetSupportedVendors() → a(ss)` — list `(vendor_id, vendor_name)` pairs
- `GetPairingSchema(vendor: s) → s` — JSON-serialized `Vec<PairingField>` describing the pairing-form fields the GUI should render for the given vendor
- `AddTv(name: s, vendor: s, config_json: s, managed: b) → (os)` — reserve a TV id and start pairing. **Returns immediately** after sync validation (unknown-vendor / `ConfigInvalidVendorConfig` surface as RPC errors); the handshake itself runs in a daemon-side spawned task. Outcome is reported via `Manager1.PairingProgress` filtered on the returned `tv_uuid` to a terminal phase (`complete` or `failed`). `complete` only fires after the per-TV `Tv1.*` interfaces are registered, so clients observing it can immediately call into the new TV without racing the registration. `config_json` carries vendor-specific connection params as JSON (e.g. `{"ip": "192.168.1.100", "mac": "AA:BB:CC:DD:EE:FF"}` for LG); `managed` controls automation participation. Per-TV lifecycle (`Remove`, `RePair`, `GetConfig`, `UpdateConfig`) lives on `Tv1` after pair.
- `ReloadConfig()` — reload config from disk
- `CancelPair(tv_id: s)` — cancel an in-flight pairing handshake. Works for both `AddTv` and `Tv1.RePair` via the daemon-side `pairing_sessions` registry. Returns `NotPairing` (typed) if the tv_id isn't currently pairing — matches the BlueZ `Adapter.RemoveDevice`-on-unknown-device convention.
- `CreateSystemProfile(name: s) → o` — mint a new UUID, register a `SystemProfile1` object at `/org/tisvlay/SystemProfile/<UUID_>` with every field inheriting, return the new path. Rejects empty / reserved (`default`) / duplicate names as `InvalidArgs`. New object surfaces via `ObjectManager.InterfacesAdded`.
- `DeleteSystemProfile(uuid: s)` — drop a system profile + its D-Bus object (fires `InterfacesRemoved`). Rejects the reserved default UUID and any UUID currently referenced by a TV's `Tv1.SystemProfile` (`AccessDenied`; message names the offending TVs).
- `DuplicateSystemProfile(source_uuid: s, new_name: s) → o` — clone an existing profile's locally-set fields under a fresh UUID + new name. Fields the source inherited stay inheriting on the copy; `ReferencedBy` starts empty.
- `DiscoverTvs(timeout_ms: u) → a(sssss)` — SSDP discovery, returns `(id, name, address, hardware_id, vendor)` tuples. Only available when compiled with the `discovery` feature.

Profile-catalog enumeration goes through `ObjectManager.GetManagedObjects()` at `/org/tisvlay` — same pattern TVs use. There's no `ListSystemProfiles` method on `Manager1`; clients walk the `SystemProfile1`-bearing object paths and read each one's `Id` / `Name` / `LocallySetFields` / `ReferencedBy` properties directly. `InterfacesAdded` / `InterfacesRemoved` from the ObjectManager root drive live updates.

Signals: `ConfigReloaded`, `PairingProgress`, `DiscoveryFound`. (Per-TV add/remove fires standard `ObjectManager.InterfacesAdded` / `InterfacesRemoved` from `/org/tisvlay`.)

`PairingProgress(tv_id: s, phase: s, message: s)` is bus-wide — any client may be running a concurrent pairing. Clients filter on the `tv_id` they got from `Manager1.AddTv` (or passed to `Tv1.RePair`) and drain to a terminal phase (`complete` or `failed`). Non-terminal phases are `awaiting_tv` and `awaiting_input`. `tisvlay-core` exposes `DaemonClient::drain_pair_to_terminal(stream, target_tv_id, on_progress) -> Result<PairTerminal>` as the canonical client-side drain — used by the GUI wizard, the `tisvlay pair` / `tisvlay re-pair` CLI subcommands, and the integration test suite.

### org.tisvlay.Tv1 (per-TV lifecycle)

The per-TV `Tv1` interface owns the methods that take a TV identifier — they read `&self.tv_id` directly from the interface impl rather than decoding it from a path argument, which avoided a class of `tv_path()` round-trip bugs around UUID hyphens.

Methods:
- `Reconnect()` — drop and re-establish the connection
- `RePair()` — refresh vendor credentials by re-running the backend's pairing handshake against the existing `vendor_config`. Top-level fields (`name`, `profile`, `managed`, `primary_input`) stay intact; only the per-vendor credentialing fields (e.g. LG's `client_key`) get replaced. **Returns immediately** after sync validation (TV not found / already pairing surface as RPC errors); the handshake itself runs in a daemon-side spawned task. Outcome via `Manager1.PairingProgress` filtered on this TV's `tv_id` to a terminal phase. Cancel an in-flight re-pair via `Manager1.CancelPair(tv_id)`. Concurrent re-pair against the same TV is rejected synchronously with `PairingFailed("already pairing")`.
- `Remove()` — drop the TV from in-memory + on-disk config and unregister every `Tv1.*` interface at this path (fires `ObjectManager.InterfacesRemoved`)
- `GetConfig() → s` — return current config (top-level + non-`Secret` vendor fields) as a JSON object suitable for prefilling an edit form
- `UpdateConfig(json_patch: s)` — atomic batch edit. Top-level keys (`name`, `managed`, `system_profile`, `primary_input`) plus any `UserInput`-visibility vendor schema field. Server-side schema-validated; vendor-field changes drop and reconnect. `Diagnostic`/`Secret` keys are rejected.

Single-field edits also flow through the per-property `Properties.Set` path — `Tv1.Name`, `Tv1.Managed`, `Tv1.SystemProfile`, `Tv1.PrimaryInput`, `Tv1.IsScreenOn`, `Tv1.Volume`, `Tv1.Muted`, `Tv1.Backlight` — which routes through the same internal helpers as `UpdateConfig` for top-level fields.

### org.tisvlay.Session1 (Session Service)

Registered on the **session bus** by `tisvlay-session` under `org.tisvlay.Session`. Mirrors the system-bus profile-catalog shape: a single `Session1` interface for lifecycle + per-TV assignment, plus one `UserProfile1` object per profile under `/org/tisvlay/UserProfile/<UUID_>`. The session-bus ObjectManager is also rooted at `/org/tisvlay` so the per-profile objects are discoverable via `GetManagedObjects()`.

Methods:
- `CreateUserProfile(name: s) → o` — mirror of `Manager1.CreateSystemProfile`; mints a UUID, registers a `UserProfile1` object, returns the new path.
- `DeleteUserProfile(uuid: s)` — mirror of `Manager1.DeleteSystemProfile`; rejects the reserved default UUID and any UUID currently referenced by a TV's `profile.user` assignment.
- `DuplicateUserProfile(source_uuid: s, new_name: s) → o` — mirror of `Manager1.DuplicateSystemProfile`.
- `GetTvUserProfile(tv_id: s) → s` — UUID of the user profile assigned to a TV (or the user-default UUID if unassigned).
- `SetTvUserProfile(tv_id: s, profile_uuid: s)` — assign a user profile by UUID; validates the UUID is known, saves to user config, live-reloads.
- `ReloadConfig()` — re-query daemon for TV list and admin baseline, reload user config from disk.

User-profile enumeration goes through the user-bus `ObjectManager.GetManagedObjects()`; no `List*` method exists.

`SetTvUserProfile`'s `UserProfileChanged(tv_id: s, profile_uuid: s)` signal carries the new UUID (was the name in v1); clients update their per-TV state from the UUID.

### Per-profile interfaces (`SystemProfile1` / `UserProfile1`)

Each profile is a first-class D-Bus object — `/org/tisvlay/SystemProfile/<UUID_>` on the system bus, `/org/tisvlay/UserProfile/<UUID_>` on the user bus. Mirrors the per-TV `Tv1` shape (`InterfacesAdded` / `InterfacesRemoved` + typed properties + `PropertiesChanged`), so clients can render and edit profiles without round-tripping JSON-encoded blobs.

Both interfaces share the same property layout:

- **Typed action properties (rw)**: one string property per inheritable field. Strings carry the serde `rename_all = "snake_case"` representation of `SessionAction` (`"power_on"`, `"power_off"`, `"screen_on"`, `"screen_off"`, `"none"`) and `AutomateWhen` (`"always"`, `"on_primary"`, `"off_primary"`). `Properties.Set` is the edit path; the daemon validates the value and writes it back. `UserProfile1` adds `IdleTimeoutSeconds: u` (unsigned 32-bit; `0` is a legitimate "disable idle" value distinct from inheriting).
- `Name: s` (rw) — friendly name. Rename = `Properties.Set("Name", new_name)`. Daemon validates non-empty + uniqueness; rejects duplicates with `InvalidArgs`.
- `Id: s` (ro, const) — the hyphenated UUID. Used as the path component (with hyphens underscored) and round-tripped through the GUI as the selection key.
- `LocallySetFields: as` (ro) — names of the fields the user has *explicitly* set on this profile (e.g. `["on_startup", "on_shutdown"]`). Drives the GUI's gutter-accent stripe + reset-glyph visibility + inherit-marker rendering. Distinguishes "this value was inherited from the parent profile" from "the user picked this value verbatim, even if it happens to equal the inherited value." Inspired by gsettings's `get_value` / `get_user_value` split — same information density, packed as a single array property.
- `ReferencedBy: as` (ro) — UUIDs of TVs that reference this profile (system bus: TVs whose `Tv1.SystemProfile` points here; user bus: TVs whose user-config `profile.user` points here). Drives the "Used by …" line in the GUI detail header and the in-use Delete rejection.

Methods (both):

- `ResetField(field: s)` — clear one field back to inheriting. Removes the entry from `LocallySetFields`; the typed property's value flips to the resolved fall-through and `PropertiesChanged` fires.
- `ResetAll()` — clear every locally-set field on this profile. One signal burst.

### Inheritance model

Typed properties always report the *resolved* value — the daemon walks the chain server-side so clients never have to implement inheritance. The chain (three layers for system profiles, four for user profiles):

```
this profile body  (Some(value) → use this, None → walk up)
        ↑
default profile body  (the reserved-UUID `00000000-…-0001`/`-0002` profile)
        ↑
hardcoded baseline  (`ResolvedSystemProfile::default()` / `ResolvedUserProfile::default()`)
```

User profiles additionally interleave an *admin baseline* layer between the user profile and the default — the daemon-owned admin defaults for the same field. Invisible on the wire; the resolved value already accounts for it.

`LocallySetFields` is the wire signal that lets the GUI distinguish "the user picked this value" from "this value was inherited"; without it, a profile that happens to match its parent's value would be indistinguishable from one inheriting. The single `as` array carries the same information per-field-bool pairs would, with half the property count and one signal per override-shape change.

### org.tisvlay.Tv1 (Base Interface)

Properties (read-only): `Id` (const), `Vendor` (const), `Address` (const), `Connected`, `Capabilities` (invalidates)

Properties (read-write): `Name`, `Managed`, `SystemProfile`, `PrimaryInput`

Properties (read-only, computed): `AutomateWhen` (from resolved system profile), `Automated` (computed gate: `managed && automate_when.should_automate(primary_input, current_input)`)

Methods: `Reconnect()`, `RePair()`, `Remove()`, `GetConfig()`, `UpdateConfig(json_patch: s)` — see the section above for full signatures.

No custom signals — uses standard `org.freedesktop.DBus.Properties.PropertiesChanged` for all state notifications. TV state is cached in the D-Bus interface structs and updated by backend subscription pushes via `InterfaceRef::get_mut()`.

### Feature Interfaces

Each feature interface is implemented on per-TV objects. Controllable state is exposed as **read-only** properties driven by backend subscription pushes — `state_forwarder` is the single emit path for `PropertiesChanged`. Writes go through explicit Set methods, matching the conventional D-Bus pattern (UPower, GNOME, BlueZ, NetworkManager). This avoids the auto-emit-vs-subscription race that affects ramping properties (notably backlight on LG OLEDs, which transitions over hundreds of ms via subscription pushes).

- **org.tisvlay.Tv1.Power** — `PowerOn()`, `PowerOff()` methods; `PowerState` (ro) property
- **org.tisvlay.Tv1.Screen** — `SetScreen(b)` method; `IsScreenOn` (ro) property
- **org.tisvlay.Tv1.Volume** — `SetVolume(y)`, `SetMuted(b)` methods; `Volume` (ro, 0-100), `Muted` (ro) properties
- **org.tisvlay.Tv1.Input** — `SwitchInput(input_id: s)` method; `CurrentInput` (ro), `AvailableInputs` (ro, invalidates) properties
- **org.tisvlay.Tv1.Backlight** — `SetBacklight(y)` method; `Backlight` (ro, 0-100) property

---

## Configuration

Config is split between **system config** (daemon-owned) and **user config** (session-owned):

- **System config**: `/var/lib/tisvlay/config.toml` — TV definitions (with per-TV system profile assignment), credentials, system profiles (daemon lifecycle events), and admin baseline user profiles. Root-owned; only `tisvlayd` reads and writes this file.
- **User config**: `~/.config/tisvlay/config.toml` — user profile definitions and per-TV user profile assignments. User-owned; `tisvlay-session` reads this. Optional: if absent, the admin baseline `user.profiles.default` from system config is inherited via D-Bus.

Both accept `--config <PATH>` to override their respective path. `tisvlayd` defaults to `/var/lib/tisvlay/config.toml`; `--session` dev mode defaults to the XDG user path.

See `crates/tisvlay-core/src/config.rs` for the full schema.

### Schema-driven vendor config

Top-level `TvConfig` fields (`name`, `managed`, `system_profile`, `primary_input`) are uniform across vendors. Vendor-specific fields (LG: `ip`, `mac`, `client_key`, `scheme`) live under `[tv.<vendor>]` and are described declaratively by the vendor's `TvConnector::config_schema()` — a `Vec<PairingField>` where each field carries a `FieldVisibility`:

| Visibility | Where it surfaces | Editable? |
|---|---|---|
| `UserInput` | `Tv1.VendorConfig`, GUI's CONFIG group, pairing wizard form, edit-mode form | yes (via `Tv1.UpdateConfig` or pairing) |
| `Diagnostic` | `Tv1.VendorInfo`, GUI's INFO group | no — backend-discovered, refresh via re-pair |
| `Secret` | nowhere (filtered out of every projection) | n/a |

Adding an editable field is a one-row schema entry — no D-Bus method changes, no CLI dispatch arms, no GUI form-rendering code. The framework projects the right view per use case.

### System Config Example

Profile sections are keyed by UUID, not name — UUIDs are the stable identity that survives renames. The reserved `00000000-0000-0000-0000-000000000001` (system) and `00000000-0000-0000-0000-000000000002` (user) UUIDs name the default profiles; the daemon ensures these sections exist on startup and rejects their deletion. Hand-editing the TOML is no longer ergonomic (UUIDs are opaque); GUI editing is the intended surface.

```toml
# /var/lib/tisvlay/config.toml  (daemon-owned)

[system]
default_profile = "00000000-0000-0000-0000-000000000001"

# System profiles — daemon lifecycle events (per-TV via profile.system assignment).
# Keyed by UUID; name lives as a body field so renames don't break references.
[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"          # action on system boot
on_shutdown = "power_off"        # action on poweroff/halt
on_reboot = "none"               # action on reboot/kexec
on_no_session = "none"           # action when all user sessions end (login screen)
on_sleep = "power_off"           # action on system suspend/hibernate
on_wake = "power_on"             # action on system resume from sleep
automate_when = "always"         # always | on_primary | off_primary

[system.profiles."a1b2c3d4-0000-0000-0000-000000000001"]
name = "shared"
automate_when = "on_primary"     # only automate when showing the PC input
                                 # (other fields inherit from default profile + baseline)

[system.profiles."a1b2c3d4-0000-0000-0000-000000000002"]
name = "guest"
on_startup = "none"
on_shutdown = "screen_off"

# Admin-set baseline for session behavior — keyed by the reserved
# user-default UUID. Users without their own config inherit this.
[user.profiles."00000000-0000-0000-0000-000000000002"]
name = "default"
on_session_lock = "screen_off"   # screen_off | power_off | none
on_session_unlock = "screen_on"  # screen_on | power_on | none
on_greeter_wake = "screen_on"    # unblank on lock screen interaction (Wayland only)
idle_timeout_seconds = 0         # 0 to disable
on_idle_timeout = "none"
on_idle_resume = "screen_on"     # action when activity resumes after idle

[[tv]]
uuid = "11111111-1111-4111-8111-111111111111"  # RFC 4122 v4, opaque, immutable
name = "Living Room G4"
vendor = "lg"
primary_input = "HDMI_1"         # which input this PC uses (vendor-agnostic)
managed = true                   # participates in automation (boot WoL, session events, shutdown)
profile.system = "00000000-0000-0000-0000-000000000001"  # which system profile this TV uses (UUID)

[[tv]]
uuid = "22222222-2222-4222-8222-222222222222"
name = "Guest Room"
vendor = "lg"
managed = true
profile.system = "a1b2c3d4-0000-0000-0000-000000000002"  # the "guest" profile by UUID

# Vendor-specific config — connection params + vendor features.
# Core has zero knowledge of these fields; each vendor crate
# defines its own config struct and deserializes internally.
[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123..."
```

### User Config Example

```toml
# ~/.config/tisvlay/config.toml  (session-owned, optional)

# User's own profiles (override or extend admin defaults).
# Same UUID-keyed shape as the system config.
[user.profiles."00000000-0000-0000-0000-000000000002"]
name = "default"
idle_timeout_seconds = 300
on_idle_timeout = "screen_off"

[user.profiles."a1b2c3d4-0000-0000-0000-00000000f001"]
name = "gaming"
on_session_lock = "none"          # don't interrupt gameplay
idle_timeout_seconds = 0
on_idle_resume = "none"

# Per-TV user profile assignments — `uuid` matches the system config's TV UUID.
[[tv]]
uuid = "11111111-1111-4111-8111-111111111111"
profile.user = "a1b2c3d4-0000-0000-0000-00000000f001"  # by UUID, not name

# guest-room inherits user.profiles."00000000-…-0002" (no override)
```

Both system and user configs use the same `[[tv]]` array-of-tables syntax. When using a single-file config, both parsers can read the same `[[tv]]` entries — each ignores fields it doesn't own (serde skips unknown fields by default).

Fields unspecified in a user profile inherit from the admin baseline user-default profile (system config), not hard-coded defaults. A user who only sets `idle_timeout_seconds = 600` on their default user profile gets all other settings from the system admin's defaults.

Key design: `TvConfig` uses `#[serde(flatten)]` to capture vendor-specific fields as raw `toml::Value`. Core has zero vendor knowledge — vendor crates define their own config structs and deserialize from the `toml::Value` internally.

---

## Power & Screen State Machine

```mermaid
stateDiagram-v2
    [*] --> STANDBY
    STANDBY --> ON : WoL / PowerOn
    ON --> BLANKED : ScreenOff
    BLANKED --> ON : ScreenOn
    ON --> STANDBY : PowerOff
    BLANKED --> STANDBY : PowerOff

    note right of BLANKED : OLED-safe\nInstant resume
    note right of STANDBY : Network may be inactive\nRequires WoL to wake
```

### Event → Action Mapping

| Event | Default Action | Config | Notes |
|-------|---------------|--------|-------|
| System boot | Power on | `on_startup` (system profile) | WoL → connect → screen on → input switch |
| Session lock | Screen off | `on_session_lock` (user profile) | Instant resume on unlock |
| Greeter wake | Screen on | `on_greeter_wake` (user profile) | User interacted with lock screen (Wayland only) |
| Session unlock | Screen on | `on_session_unlock` (user profile) | Skipped if GreeterWake already unblanked |
| User idle timeout | Screen off | `on_idle_timeout` (user profile) | Fires at minimum across all TVs |
| Activity resumes | Screen on | `on_idle_resume` (user profile) | After idle timeout |
| System suspend | Power off | `on_sleep` (system profile) | Network unavailable during sleep |
| System resume | Power on | `on_wake` (system profile) | Backend handles WoL retry for lost packets |
| All sessions end | None | `on_no_session` (system profile) | Login screen / after logout |
| System shutdown | Power off | `on_shutdown` (system profile) | poweroff or halt |
| System reboot | None | `on_reboot` (system profile) | reboot or kexec; TV stays on for BIOS/TTY |

All lifecycle events accept any `SessionAction` value: `power_on`, `power_off`, `screen_on`, `screen_off`, or `none`. Different TVs can respond differently to the same event via per-TV profile assignments (e.g., gaming TV ignores session lock via user profile; guest TV only blanks on shutdown via system profile).

All automation (both daemon system events and session user events) is gated by the `Automated` property, which composes `managed`, `automate_when` (from system profile), `primary_input` (from config), and the TV's current input. When `automate_when = on_primary` and the TV is showing a game console, automation is skipped. Manual actions from the tray and CLI always execute regardless of the gate.

| `managed` | `automate_when` | TV on primary? | `Automated` |
|:---------:|:---------------:|:--------------:|:-----------:|
| false | any | any | **false** |
| true | always | any | **true** |
| true | on_primary | yes | **true** |
| true | on_primary | no | **false** |
| true | on_primary | unknown (TV off) | **true** (benefit of doubt) |
| true | off_primary | yes | **false** |
| true | off_primary | no | **true** |

### Reliable screen-state actions

`tv_manager::screen_on` and `tv_manager::screen_off` are **inherently reliable**. Both dispatch through a verify-retry-escalate loop rather than fire-and-forget. The reliability is not a per-event opt-in or QoS tier; it's the only behavior the methods have.

**Verify-retry-escalate per direction:**

| Direction   | Pre-flight wake gate                  | Verify target          | On retry exhaustion       |
|-------------|---------------------------------------|------------------------|---------------------------|
| `screen_on` | `power_on` if cached in wake-preflight set | `Active` / `ScreenSaver` | Escalate to `power_on`    |
| `screen_off`| `power_on` if cached in wake-preflight set, then settle | `ScreenOff`           | Warn + accept (TV reachable, panel refused — no power-cycle) |

Cached state is `ManagedTv::last_known_power_state` (raw [`PowerState`](../crates/tisvlay-core/src/state.rs) enum, populated by subscription pushes and live `getPowerState` queries). The pre-flight wake set is defined by [`crate::power_state::needs_wake_preflight`](../crates/tisvlayd/src/power_state.rs):

- `Standby` / `Suspend` — TV is deeply off; in-band `screen_on` would stall on the half-asleep WS.
- `ActiveStandby` — transient substate after a remote-press power-off; LG firmware ACKs `screen_*` but won't actually transition.
- `Unknown` — no subscription push received yet; safest to assume off.
- (`ScreenOff` deliberately **excluded** — it's the target state for `screen_off`, and `screen_on` from `ScreenOff` is exactly the lockscreen-wake fast path the `with_retry` "knock" mechanism is tuned for.)

**Per-backend tuning.** Verify-delay and max-attempts come from `TvBackend::verify_retry_config() -> RetryConfig`, defaulted to `{ verify_delay: 300ms, max_attempts: 2 }` (LG-tuned). Future USB-CEC / RS-232 backends are expected to override for their different latency characteristics.

**Event-driven waits, not sleep-poll.** Each `ManagedTv` owns a `tokio::sync::watch::Sender<PowerState>` fired by every cached-state writer (subscription task + live `getPowerState` refresh). The verify-retry loops route through `TvManager::await_power_state(tv_id, timeout, predicate)`, which subscribes to the watch and returns the first matching transition (or the last seen value on timeout). The retry budget becomes an upper bound, not a guaranteed wait — fast TVs return immediately on the subscription push.

**LG firmware quirk fallback.** If `screen_off`'s post-wake settle phase times out (LG firmware honored a very recent remote-off and refused to fully wake), the TV is sitting in `ActiveStandby` with the panel already dark. The user's screen-off intent is functionally satisfied, so accept with an info log rather than burning more retries on `turnOffScreen` rejections. See [`docs/lg/firmware-quirks.md`](lg/firmware-quirks.md) for the full diagnosis.

**D-Bus surface.** Unchanged from pre-reliability work: `Tv1.Power.PowerOn` / `PowerOff`, `Tv1.Screen.SetScreen(b)`. Reliability is implementation-side; clients (tray, GUI, CLI, session service) get reliable dispatch implicitly without negotiating tiers.

---

## CLI Interface

```
tisvlay - Multi-Vendor TV Control

USAGE:
    tisvlay [OPTIONS] <COMMAND>

COMMANDS:
    power           Power control (on/off)
    screen          Screen control (blank/unblank)
    input           Switch HDMI input (lists inputs when no ID given)
    volume          Volume control
    mute            Mute/unmute (on/off/toggle, default: toggle)
    backlight       Backlight control
    status          Show TV status (includes managed flag)
    pair            Pair a new TV
    re-pair <ID>    Refresh vendor credentials in place (preserves name, profile, etc.)
    list            List configured TVs (shows [unmanaged] suffix)
    remove          Remove a TV
    manage          Show or set automated management for a TV (true/false)
    profile         Profile management
      user          User profile management (requires tisvlay-session)
        list        List available user profiles
        get         Show user profile assigned to a TV
        set <NAME>  Assign a user profile to a TV
      system        System profile management
        list        List available system profiles
        get         Show system profile assigned to a TV
        set <NAME>  Assign a system profile to a TV

OPTIONS:
    --tv <ID>       Target TV (default: first configured)
    --user          Use session bus (development mode)

EXAMPLES:
    tisvlay power on
    tisvlay screen off
    tisvlay --tv office input HDMI_2
    tisvlay input                                                   # list available inputs
    tisvlay pair 192.168.1.100 --name "Living Room" --vendor lg --wake-id AA:BB:CC:DD:EE:FF
    tisvlay pair 192.168.1.101 --name "Office" --vendor lg --unmanaged
    tisvlay re-pair living-room                                     # refresh expired credentials
    tisvlay volume 30
    tisvlay backlight 80
    tisvlay manage                                                      # show current
    tisvlay manage true
    tisvlay manage false --tv office
    tisvlay profile user list
    tisvlay profile user set gaming --tv living-room
    tisvlay profile system list
    tisvlay profile system set guest --tv guest-room
```

---

## System Tray Menu Structure

Icons convey managed state: `tvconnected` (managed + connected), `tvtrusted` (unmanaged + connected), `tvdisconnected` (disconnected regardless of managed state).

Connected TV submenu:
```
▶ Living Room G4  [tvconnected]
  ├─ Power Off
  ├─ Profile ▶ User: gaming ▶  (RadioGroup: gaming ✓, default)   [settings-configure]
  │            System: default ▶  (RadioGroup: default ✓, guest)   [settings-configure]
  ├─ Unmanage
  ├─ Input ▶  (RadioGroup: HDMI 1, HDMI 2 ✓, HDMI 3)   [input-tvremote-symbolic]
  ├─ Volume: 60
  │  ├─ Volume Up (+10)
  │  ├─ Volume Down (-10)
  │  └─ Mute / Unmute
  ├─ Backlight: 80
  │  ├─ Backlight Up (+10)
  │  └─ Backlight Down (-10)
  └─ Screen Off / Screen On
```

Disconnected TV submenu:
```
▶ Office C3  [tvdisconnected]
  ├─ Power On (Wake)
  ├─ Profile ▶ User: default ▶  (RadioGroup: default ✓)
  │            System: default ▶  (RadioGroup: default ✓)
  └─ Unmanage
```

Unmanaged connected TV submenu:
```
▶ Guest TV  [tvtrusted]
  ├─ Power Off
  ├─ Profile ▶ User: default ▶
  │            System: default ▶
  ├─ Manage
  ├─ Input ▶
  └─ ...
```

Bottom:
```
──────────
Refresh
──────────
Quit
```

Scroll wheel adjusts volume or backlight for the TV last interacted with via a volume/backlight control; falls back to the first connected TV.
Screen Off/On uses `backlight_item` (same as Backlight Up/Down) — clicking it contextualizes the scroll wheel to backlight, since screen off is the discrete pole of the backlight axis.

---

## Implementation Phases

### Phase 1: Core Library + CLI (MVP) - COMPLETE

**Goal**: Basic TV control from command line.


### Phase 2: System Daemon - COMPLETE

**Goal**: Background service with D-Bus API.


### Phase 3: User Session Integration - COMPLETE

**Goal**: React to desktop events.
### Phase 4a: CLI D-Bus Migration - COMPLETE

**Goal**: CLI communicates with daemon via D-Bus.

### Phase 4b: Tray App - COMPLETE

**Goal**: Quick access from system tray.

1. `tisvlay-tray`: ksni-based StatusNotifierItem with per-TV submenus
2. Power/screen/input/volume/backlight/mute controls
3. Scroll wheel adjusts volume or backlight (context-dependent)
4. Signal-driven updates via `PropertiesChanged`/`InterfacesAdded`/`InterfacesRemoved` signals with 100ms debounce and 30s fallback poll

### Phase 4C: System vs User Config Split - COMPLETE

**Goal**: Separate daemon-owned and user-owned configuration.

1. `SystemConfig` (daemon) and `UserConfig` (session service) with separate ownership and paths
2. Profile field-level inheritance: user profile fields override admin defaults, unset fields fall through
3. `on_idle_resume` added to `UserProfileConfig` (action when activity resumes after idle)
4. `org.tisvlay.Session1` D-Bus interface on session bus: `ListUserProfiles`, `GetTvUserProfile`, `SetTvUserProfile`, `GetEffectiveUserProfile`, `ReloadConfig`
5. Daemon gains `org.freedesktop.login1` monitoring: `PrepareForShutdown` (inhibitor lock), `SessionNew`/`SessionRemoved` for `on_no_session`
6. Boot-wake refactored: retry-connect loop with WoL resend replaces blind sleep delay
7. Session service event replay after late daemon connect
8. Tray gains User Profile and System Profile submenus per TV
9. Manager1 additions: `SetTvOption`, `ListSystemProfiles`, `GetUserProfileDefault`, `ListUserProfiles`

### Packaging - COMPLETE

**Goal**: Install the full stack on any distro.

**NixOS** (`services.tisvlay.enable = true`) — see [packaging/nix/README.md](../packaging/nix/README.md).

**Multi-distro** — see [packaging/README.md](../packaging/README.md).

**GitHub releases**: see [`.github/workflows/release.yml`](../.github/workflows/release.yml) — triggers on `v*` tags.

**crates.io**: each crate published independently; `cargo install` provides binaries only (no service files)

**Common**:
- `tisvlayd`/`tisvlay-session` `--config` flag for explicit config path
- System daemon uses `StateDirectory` (`/var/lib/tisvlay/`) for writable config (supports `ProtectHome=true`)
- D-Bus policy in `packaging/systemd/org.tisvlay.Daemon.conf`
- User services auto-start on `graphical-session.target`

### Phase 5A: `managed` Flag + CLI Profile Parity - COMPLETE

**Goal**: Let users designate which TVs participate in automation without unpairing.

1. `managed: bool` added to `TvConfig` (required TOML field, no serde default) and `TvInfo`
2. `TvConfig::new(id, name, vendor, managed)` — all four required; UX default lives in CLI only
3. Daemon enforces `managed` in all automation paths: boot WoL, session events, shutdown
4. `TvEntry = (bool, Arc<Mutex<ManagedTv>>)` — managed flag stored outside per-TV mutex; `list_managed_tv_ids()` scans with only the outer read lock
5. `Managed` D-Bus property exposed on `org.tisvlay.Tv1`; `SetTvOption` handles `managed` key
6. `AddTv` D-Bus signature extended with `managed: b` parameter
7. Tray: icon reflects managed state (`tvconnected`/`tvtrusted`/`tvdisconnected`); Manage/Unmanage toggle in every TV submenu
8. CLI: `manage [true|false]` command (no arg shows current status); `list` shows `[unmanaged]` suffix; `pair --unmanaged` flag; `profile list/get/set` subcommands via `SessionClient`
9. Session service: profiles built for all paired TVs at load; event dispatch filtered live per-event

### Phase 5B: Session Service — Live `managed` Filtering - COMPLETE

**Goal**: Toggle managed at runtime without requiring a service reload.

1. `event_handler` queries `daemon.list_tvs()` before each dispatch to get the current managed set
2. Fallback on daemon unreachable: skip dispatch entirely (empty set) rather than leaking unmanaged TVs

### Phase 5C: `ObjectManager` for Bulk TV Discovery - COMPLETE

**Goal**: Replace 1 + 6N D-Bus round-trips with a single `GetManagedObjects` call.

1. `org.freedesktop.DBus.ObjectManager` registered at `/org/tisvlay` (ancestor of all tisvlay objects)
2. `GetTvs()` removed from `org.tisvlay.Manager1` — superseded by `GetManagedObjects`
3. `DaemonClient.list_tvs()` rewritten: single round-trip, no proxy objects created, O(1) D-Bus cost regardless of TV count
4. `get_bool(key, default)` uses explicit defaults: `Connected → false`, `Managed → true`
5. Diagnostic logging: `debug!` per entry on property fallbacks; `warn!` when `Id` is empty

### Per-TV System Profiles Refactor - COMPLETE

**Goal**: Per-TV daemon lifecycle behavior via system profiles, symmetric with user profiles.

1. Renamed `ProfileConfig` → `UserProfileConfig`, `ResolvedProfile` → `ResolvedUserProfile` across all crates
2. D-Bus renames: `GetSystemProfile` → `GetUserProfileDefault`, `GetSystemProfiles` → `ListUserProfiles`, `ListProfiles` → `ListUserProfiles`, `GetTvProfile` → `GetTvUserProfile`, `SetTvProfile` → `SetTvUserProfile`, `GetEffectiveProfile` → `GetEffectiveUserProfile`
3. Added `SystemProfileConfig` and `ResolvedSystemProfile` with fields: `on_startup`, `on_shutdown`, `on_reboot`, `on_no_session`
4. Config restructured: `[daemon]` → `[system.profiles.*]`, `[profiles.*]` → `[user.profiles.*]`, `profile = "name"` → `profile.user = "name"`, new `profile.system = "name"` on `TvConfig`
5. Removed `DaemonConfig` — replaced by per-TV system profile resolution
6. Daemon dispatches lifecycle events per-TV using each TV's resolved system profile
7. `SetDaemonOption` removed; system profile set via `SetTvOption` key `"profile.system"`
8. `ListSystemProfiles()` added to Manager1; `SystemProfile` property added to Tv1
9. Tray gains dual submenus: "User Profile" and "System Profile" per TV
10. CLI: `tisvlay profile user list/get/set` and `tisvlay profile system list/get/set`

### Brightness / Volume Sync (researched and scrapped)

[PR #34](https://github.com/ruffsl/TisVlay/pull/34) carried experimental volume sync via PipeWire and brightness sync via KWin Wayland + PowerDevil D-Bus + a virtual backlight kernel module that ultimately did not merge to main. PowerDevil binary patches against Plasma's release cadence, the non-bidirectional `kde_external_brightness_v1` protocol, HDR-vs-SDR code-path divergence, and gain-stage chaining in PipeWire mirror sinks made the maintenance burden outweigh the capability. The granular commit history is preserved in that PR; see [docs/ideas.md](ideas.md) for the rationale and the event-driven power-management direction the next merge cycle is taking.

## Future Work

Forward-looking work that hasn't shipped yet. No fixed sequencing — entries land in whichever order makes sense.

### GUI Configuration

`tisvlay-gui` is largely built and packaged — see [docs/plan.md](plan.md) for the full milestone status, and [packaging/README.md](../packaging/README.md) for the shipping channels. The TVs pane (master/detail with live-state controls + edit-mode pencil), pairing wizard (discovery streaming + schema-driven step-2 form), and About pane shipped through M3 + Plan-6 follow-on work. The Profiles pane, Session pane, NixOS module's `enableGui` flag, and the XDG `.desktop` entry remain as M3.5 / M4 / M5 / M6 follow-ups.

### Event-Driven Power Management

Today's `tisvlay-session` listener path drives `Tv1.Power.SetIsScreenOn` / `PowerOff` from session-bus events (screensaver, Wayland idle, logind, idle timer). A follow-up merge replaces this with per-DE adapters (KWin idle/DPMS, wlroots output management, X11 XScreenSaver, logind fallback) that emit a unified `ScreenBlank` / `ScreenWake` / `Sleep` / `Wake` event surface the daemon dispatches to `Tv1.Power.*`. Hard requirements (sub-100 ms lockscreen-snap-back, blank-vs-off distinction, multi-TV per-EDID-connector, detached-TV idle-timer fallback) and the "what NOT to do" gotchas are captured in [docs/ideas.md](ideas.md).

---

## Known Challenges & Mitigations

| Challenge | Mitigation |
|-----------|------------|
| TV ignores WoL during pixel refresh | Retry WoL with backoff; inform user if TV unresponsive |
| Newer firmware requires TLS (wss://) | Default to port 3001; fallback to 3000; accept self-signed certs |
| WebSocket closed on TV idle (~1-2 min) | LG backend auto-reconnects transparently before each command |
| WoL lost after system resume | Backend resends WoL between connect retries |
| PowerOff + SessionLocked race on sleep | Daemon `powered_off` flag prevents blocking connect to dead TV |
| Screen on/off returns 500 when idempotent | LG backend treats application errors as success for screen commands |
| DHCP IP changes | Use MAC for WoL; re-discover via SSDP |
| Greeter wake unavailable on X11 | Wayland `ext-idle-notify-v1` only; falls back gracefully |
| GUI is Wayland-only | `tisvlay-gui` builds against `slint = ["backend-winit-wayland"]` and `wl-clipboard-rs`; X11 sessions run the daemon, CLI, and tray (ksni) but the settings window doesn't launch. Aligns with `tisvlay-session`'s Wayland-best-effort posture above and trims `x11rb` from the dep graph. See [build.md](build.md). |
| Backlight write requires workaround | SSAP subscriptions work for reading; writing requires Luna `createAlert` workaround (direct setSystemSettings returns 401) |

---

## Push-Based State Pipeline

State updates flow end-to-end without polling:

```
TV remote/app → WebSocket push → LG backend → daemon StateChangeEvent
    → InterfaceRef cached property update → PropertiesChanged signal
    → tray handle.update()
```

### LG Backend SSAP Subscriptions

The LG SSAP protocol supports `subscribe` requests on the same URIs used for `request` (GET). The TV pushes updates over the existing multiplexed WebSocket whenever a value changes. Subscribed endpoints:

- `ssap://com.webos.service.tvpower/power/getPowerState` (power state)
- `ssap://audio/getVolume` (volume level)
- `ssap://audio/getMute` (mute state)
- `ssap://tv/getExternalInputList` (available inputs)
- `ssap://com.webos.applicationManager/getForegroundAppInfo` (current input/app)
- `ssap://settings/getSystemSettings` with `category: "picture", keys: ["backlight"]` (backlight level)

See [docs/lg/webos.md](lg/webos.md) for full SSAP probe results and subscription behavior details.

After connecting and registering, the LG backend sends subscribe requests for all of the above. A background reader task on the multiplexed WebSocket routes subscription updates (identified by request ID, not URI) to a state change channel and command responses to their respective callers via oneshot channels.

### Vendor-Agnostic Design

The `TvConnection` trait provides an optional `subscribe_state()` method with a default not-supported implementation. Backends that support push (LG) override it to return an `mpsc::UnboundedReceiver<TvState>`. The daemon checks at connection time: if subscriptions are available, use them; otherwise, the 30s fallback poll in the tray provides resilience. This keeps the tray always signal-driven regardless of backend capability.

### Daemon Layer

After establishing a connection, the daemon calls `subscribe_state()` on the `TvConnection`. If the backend supports push (LG does), a spawned task forwards received `TvState` snapshots as `StateChangeEvent::SubscriptionUpdate`s through an internal channel. The `state_change_forwarder` task receives these and updates cached property fields on all per-TV D-Bus interface structs via `InterfaceRef::get_mut()`, emitting standard `PropertiesChanged` signals only for properties that actually changed.

Write commands go through explicit Set methods (`SetVolume`, `SetMuted`, `SetBacklight`, `SetScreen`) which dispatch to `TvManager` and return after the backend acknowledges. The cached property field is **not** mutated by the method — `state_forwarder` updates it and emits `PropertiesChanged` only when the TV's subscription confirms the new value. Single emit path, no auto-emit-vs-subscription race; convergence-gating clients (the GUI's `OptimisticSlider`, the tray's `Reconciler`) see exactly one stream of authoritative state changes per property.

### Per-Device Network Monitoring

The daemon uses kernel netlink to detect network state changes per TV, scoped to the exact NIC carrying traffic. No NetworkManager dependency.

A single `rtnetlink` multicast connection subscribes to link, route, and address events. Carrier up/down fires within milliseconds of a cable pull or NM software disconnect. Route/address changes are coalesced and trigger re-resolution of all TV → NIC mappings.

When a network-transport TV connects, the daemon resolves its IP to a NIC via `RTM_GETROUTE` (equivalent to `ip route get`) and stores the result on `ManagedTv::monitored_interface`. Carrier events are then scoped to only the TVs routed through the affected NIC — virtual bridges (virbr0, docker0) don't mask real disconnects.

When a route shifts to a different NIC (e.g., wired → Wi-Fi failover), the daemon checks `TvConnector::survives_source_ip_change()`. TCP/TLS-based protocols (WebSocket) return `false` — the connection is bound to a source IP and must be closed and re-established via the new interface. Future QUIC-based backends could return `true`.

All checks use `Transport::is_network_dependent()` — non-network transports (serial, USB, CEC) are unaffected. A per-TV `reconnecting` flag prevents carrier/route events from racing with in-flight WoL retry loops during system wake.

See [docs/watchers/network.md](watchers/network.md) for the full architecture, pitfalls discovered (RTM_F_LOOKUP_TABLE, IPv6 link-local persistence, NM watcher removal), and failover behavior.

See [docs/lg/benchmarks/wake-reconnect.md](lg/benchmarks/wake-reconnect.md) for measured timing data.

### Daemon Lifecycle

On **shutdown** (SIGINT/SIGTERM), the daemon closes all TV WebSocket sessions gracefully before releasing the D-Bus name. This ensures TVs drop their protocol sessions immediately — without it, the TV holds the stale session until TCP keepalive timeout, causing "Connection reset" on the next daemon start.

On **warm restart** (process restart, not system boot), the daemon proactively connects to all configured TVs. A boot marker in `/run/tisvlay/` (tmpfs, cleared every boot) distinguishes system boot from service restart. No WoL or startup actions are fired — the TVs are already on. Connection retry for stale sessions is handled internally by the vendor backend (e.g., LG retries up to 5 times with 1s interval).

### Tray / GUI Layer

D-Bus signal subscription, service lifecycle watching (daemon + session), signal debouncing, and fallback polling are encapsulated in `StateWatcher` (`tisvlay-core`, behind the `dbus` feature gate). The watcher emits a `StateEvent` enum — consumers provide only the UI-specific response.

The tray's event loop is ~45 lines: create a `StateWatcher`, match on events, and call `refresh_state()` or handle service appear/disappear. The future GUI will use the same `StateWatcher` with its own event handler.

Signals within a 100ms window are coalesced before the watcher emits `DaemonStateChanged`. A 30s fallback poll provides resilience. `NameOwnerChanged` detects services appearing/disappearing on the bus for sub-second discovery.

---

## References

- [Testing Guide](testing.md) - Mock backend, test harnesses, coverage, observability
- [Firewall Configuration](firewall.md) - SSDP discovery firewall setup
- [LG WebOS API](https://github.com/chros73/bscpylgtv/tree/master/docs)
- [zbus Documentation](https://docs.rs/zbus)
- [ksni Documentation](https://docs.rs/ksni)
- [Slint Documentation](https://docs.slint.dev)
