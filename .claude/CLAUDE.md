# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Currently supports LG WebOS TVs, with a vendor-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU does not support HDMI-CEC, so network-based control via vendor protocols (e.g., LG WebOS SSAP) is required.

See [docs/design.md](docs/design.md) for the full architecture, D-Bus interface specs, config schema, CLI reference, and implementation phase history.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## What's Next

- Event-driven power-management redesign — replace today's session-bus listener path with per-DE adapters (KWin / wlroots / X11 / logind fallback). See [docs/ideas.md](../docs/ideas.md) "Event-driven power-management redesign" for design notes carried forward from the dev-branch sync research.
- `tisvlay-gui`: Slint-based settings window
- TV discovery and pairing wizard (with managed checkbox)
- Per-TV configuration and session behavior settings
- Input-aware automation (`primary_input`, `automate_when`) — see design.md § Power & Screen State Machine

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── tisvlay-core/              # Core library (vendor-agnostic traits & types)
│   │   └── src/
│   │       ├── lib.rs            # Public API exports
│   │       ├── traits.rs         # TvBackend, TvConnection, TvConnector, TvDiscovery
│   │       ├── capabilities.rs   # Capability bitflags
│   │       ├── pairing.rs        # Unified pairing state machine
│   │       ├── config.rs         # TOML config (XDG paths), SystemProfileConfig, UserProfileConfig
│   │       ├── dbus_paths.rs     # D-Bus path constants and helpers
│   │       ├── dbus_client.rs    # D-Bus client, StateWatcher, signal helpers (feature-gated)
│   │       ├── logging.rs        # Shared logging init with journald (feature-gated)
│   │       ├── state.rs          # TvState, PowerState, InputInfo
│   │       ├── wol.rs            # Wake-on-LAN
│   │       └── error.rs          # Error types
│   │
│   ├── tisvlay-lg/                # LG WebOS vendor implementation
│   │   └── src/
│   │       ├── lib.rs            # Public exports
│   │       ├── backend.rs        # LgConnector, LgConnection trait impls
│   │       ├── client.rs         # WebOsClient - high-level API
│   │       ├── connection.rs     # WebSocket with rustls TLS
│   │       ├── protocol.rs       # SSAP message types
│   │       ├── endpoints.rs      # API endpoint constants
│   │       ├── config.rs         # LgVendorConfig
│   │       └── discovery.rs      # SSDP TV discovery (feature-gated)
│   │
│   ├── tisvlay-mock/              # In-memory mock backend (optional, for dev/tests)
│   │   └── src/
│   │       ├── lib.rs            # Public exports, VENDOR_ID, new_test_parts/new_production_parts
│   │       ├── backend.rs        # MockBackend (metadata)
│   │       ├── connector.rs      # MockConnector (factory, pairing with PairingOutcome)
│   │       ├── connection.rs     # MockConnection (in-memory state + push subscriptions)
│   │       ├── config.rs         # MockVendorConfig (initial state + PairingShorthand)
│   │       ├── discovery.rs      # MockDiscovery (reads from shared MockState)
│   │       ├── handle.rs         # MockHandle (test-driven discovery + pairing control)
│   │       ├── pairing.rs        # PairingOutcome, PairingShorthand
│   │       └── state.rs          # MockState (shared between handle, connector, discovery)
│   │
│   ├── tisvlay-test/              # OS-environment fakes (dev-only, never shipped)
│   │   └── src/
│   │       ├── lib.rs            # Re-exports: PrivateBus, traced_test, tracing_mock
│   │       ├── dbus.rs           # PrivateBus (private dbus-daemon per test binary)
│   │       ├── fake_logind.rs    # FakeLogind (login1.Manager stub with PrepareForSleep)
│   │       └── fake_screensaver.rs # FakeScreenSaver (ScreenSaver stub with ActiveChanged)
│   │
│   ├── tisvlay-harness/           # Binary-lifecycle test fixtures (dev-only)
│   │   └── src/
│   │       ├── lib.rs            # Module index
│   │       ├── common.rs         # Shared setup (private-bus, tracing, mock registration)
│   │       ├── daemon.rs         # TestDaemon — boots tisvlayd on a private bus
│   │       ├── session.rs        # TestSession — boots tisvlay-session + composed TestDaemon
│   │       └── tray.rs           # TestTray — placeholder for tray integration tests
│   │
│   ├── tisvlay-backends/          # Backend registry (composition point)
│   │   └── src/
│   │       └── lib.rs            # BackendRegistry, vendor registration, MockHandle re-export
│   │
│   ├── tisvlay-cli/               # CLI tool
│   │   └── src/
│   │       └── main.rs           # clap-based CLI
│   │
│   ├── tisvlayd/                  # System daemon (lib + bin)
│   │   └── src/
│   │       ├── lib.rs            # Library root (pub modules + __internals re-exports for tisvlay-harness)
│   │       ├── main.rs           # Entry point (thin, delegates to lib)
│   │       ├── dbus/             # D-Bus interfaces (Manager1, Tv1.* with cached properties)
│   │       ├── dbus/setup.rs     # build_dbus_connection helper (BusKind lives in tisvlay-core)
│   │       ├── state_forwarder.rs # Backend state → D-Bus PropertiesChanged pipeline
│   │       ├── logind.rs         # logind monitoring (shutdown inhibitor, session tracking)
│   │       ├── netlink.rs        # Kernel netlink: route resolution + carrier/route monitoring
│   │       ├── registry.rs       # Re-exports from tisvlay-backends
│   │       └── tv_manager.rs     # TV connection pool, profile dispatch, disk serialization
│   │
│   ├── tisvlay-session/           # User session service (lib + bin)
│   │   └── src/
│   │       ├── lib.rs            # Library root (SessionState, event_handler, __internals)
│   │       ├── main.rs           # Entry point (thin, delegates to lib)
│   │       ├── session_dbus.rs   # org.tisvlay.Session1 D-Bus interface
│   │       ├── session_monitor.rs # Session lock/unlock detection
│   │       ├── wayland_idle.rs   # Wayland greeter wake detection
│   │       └── idle.rs           # Idle timeout detection
│   │
│   └── tisvlay-tray/              # System tray app
│       └── src/
│           ├── main.rs           # Entry point, signal watcher, command processor
│           ├── tray.rs           # ksni Tray impl, menu building, TrayCommand
│           └── state.rs          # CachedTvState, fetch_all_state()
│
├── packaging/
│   ├── nix/
│   │   ├── package.nix           # Nix package derivation (rustPlatform.buildRustPackage)
│   │   └── module.nix            # NixOS module (services.tisvlay.enable)
│   ├── scripts/
│   │   ├── postinstall.sh        # Distro package post-install (systemd + D-Bus reload)
│   │   └── preremove.sh          # Distro package pre-remove (stop services)
│   └── systemd/
│       ├── tisvlayd.service           # Systemd system service
│       ├── tisvlay-session.service    # Systemd user service
│       ├── tisvlay-tray.service       # Systemd user service (tray)
│       ├── org.tisvlay.Daemon.service # D-Bus activation service
│       └── org.tisvlay.Daemon.conf    # D-Bus policy configuration
│
├── docs/
│   ├── design.md                 # Full architecture design
│   ├── firewall.md               # SSDP firewall configuration
│   └── watchers/
│       └── network.md            # Network monitoring: carrier, route resolution, pitfalls
│
└── .claude/CLAUDE.md             # This file
```

## Key Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| WebSocket | `tokio-tungstenite` + `rustls` |
| JSON | `serde`, `serde_json` |
| Config | `toml`, `directories` |
| CLI | `clap` |
| D-Bus | `zbus` |
| System tray | `ksni` |
| Wayland | `wayland-client`, `wayland-protocols` |
| Errors | `thiserror`, `anyhow` |
| Logging | `tracing`, `tracing-subscriber`, `tracing-journald` |
| WoL | `socket2` |
| Netlink | `rtnetlink`, `netlink-packet-route`, `netlink-packet-core` |

## Known Quirks

- Newer firmware requires wss:// (port 3001) with self-signed cert; uses `rustls` with custom `InsecureCertVerifier`
- TV may ignore WoL during pixel refresh
- Need "TV On via Wi-Fi" enabled in TV settings
- ~1-5 second delay needed between WoL and first command
- TV closes WebSocket on standby; need reconnection logic
- Backlight IS SSAP-subscribable (via `getSystemSettings` with `category: "picture", keys: ["backlight"]`); writing still requires Luna `createAlert` workaround (direct setSystemSettings returns 401)
- SSAP subscription updates carry the original request ID, not the URI -- routing must use a request-ID-to-URI map
- Multiplexed WebSocket: background reader routes subscription updates to state channel and command responses to callers; SSAP subscriptions (power, volume, mute, inputs, foreground app, backlight) push state in real time
- LG pairing is two-phase: TV sends `Response` (prompt shown), then `Registered` (user accepted). `request_wait()` on the multiplexed WebSocket handles the second phase without re-sending
- `Transport` enum on `TvConnector`: Network, Serial, Usb, Cec. Daemon scopes carrier/route events to network-dependent backends only
- `TvConnector::survives_source_ip_change()`: backends declare whether their protocol survives client IP changes (WebSocket: false, future QUIC: true). Daemon uses this on NIC failover to decide whether to reconnect
- Per-TV `reconnecting` flag prevents carrier/route events from racing with in-flight WoL retry loops

## Quick Reference

### Running the Stack

```bash
# Development (session bus):
tisvlayd --session
tisvlay-session --user
tisvlay-tray --user

# Production (system bus, as systemd services):
sudo tisvlayd
systemctl --user enable --now tisvlay-session
systemctl --user enable --now tisvlay-tray
```

### Testing with busctl

```bash
# Manager interface
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ListSystemProfiles

# List all objects + properties (single round-trip for all TV state)
busctl --user call org.tisvlay.Daemon /org/tisvlay org.freedesktop.DBus.ObjectManager GetManagedObjects

# Per-TV properties (replace living_room with TV ID)
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/living_room
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Connected
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Volume

# Writable properties (set via Properties.Set)
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Volume y 30
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Muted b true
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Screen IsScreenOn b false
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Backlight Backlight y 80

# Methods (complex operations that aren't simple property sets)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Power PowerOff
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Power PowerOn
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Input SwitchInput s HDMI_2

# Monitor PropertiesChanged signals
busctl --user monitor org.tisvlay.Daemon
```

### NixOS Installation

```nix
# flake.nix input
inputs.tisvlay.url = "github:ruffsl/tisvlay";

# configuration.nix
{ inputs, ... }: {
  imports = [ inputs.tisvlay.nixosModules.default ];
  services.tisvlay.enable = true;
}
```

This sets up:
- `tisvlayd` system service (runs as `tisvlay` user, config in `/var/lib/tisvlay/`)
- `tisvlay-session` user service (auto-starts on graphical session)
- `tisvlay-tray` user service (auto-starts on graphical session)
- D-Bus policy and CLI in `$PATH`

## Tasks & Commands (`just`)

All project commands live in the `justfile` at the repo root — it is the
single source of truth. prek hooks, VSCode tasks, and CI workflows all
dispatch through `just` so command strings are defined in exactly one place.

```bash
just              # list all recipes, grouped by purpose
just check        # fmt + build + clippy + test (all fast checks)
just build        # cargo build --profile test --workspace --all-targets --all-features
just clippy       # cargo clippy ... -- -D warnings
just test         # cargo nextest run + cargo test --doc
just test-ci      # nextest with the 'ci' profile (JUnit XML output)
just daemon-start # start tisvlayd on the session bus (dev mode)
just cov          # coverage summary
just cov-ratchet  # coverage ratchet check (CI)
just package-deb  # build the .deb package
```

`just` is provided by the Nix dev shell. Outside Nix, install with
`cargo install just` or your distro package manager.

## Linting & CI

All checks are defined in `prek.toml`, which dispatches to `just` recipes.
Hook ids match the corresponding `just` recipe names. Hooks are ordered by
priority:

- **0**: file checks (trailing whitespace, EOF, TOML/YAML validation, merge conflicts, large files)
- **1**: `fmt` → `just fmt`
- **2**: `build` → `just build` (test profile, all targets, all features)
- **3**: `clippy` → `just clippy` (warnings denied)
- **4**: `test` → `just test`

Build and clippy use `--profile test` so `cargo test` reuses all build artifacts with zero recompilation.

```bash
# Run all checks (local pre-commit)
prek run -a

# Run only fast file checks + fmt (skip compilation)
prek run -a --skip build --skip clippy --skip test

# Run a specific hook
prek run -a clippy

# CI runs file checks via prek, then just build/clippy/test directly for live output
```

### Dead Code Detection

Rust's `dead_code` lint only catches unused *private* items. Public items are invisible
to it — the compiler assumes external consumers exist. Since this workspace has no
external consumers, use `cargo-workspace-unused-pub` for cross-crate unused pub detection:

```bash
# Ad-hoc check (generates SCIP index via rust-analyzer, may take a moment)
cargo workspace-unused-pub

# Review output manually — false positives include:
# - Trait impl methods required by frameworks (rustls, ksni, zbus proxy traits)
# - Builder methods reserved for future use (discovery wizard, GUI)
```

Not in the `prek` pipeline (requires rust-analyzer, slower than clippy). Run periodically
after major refactors to catch orphaned public API surface.

### Testing

See [docs/testing.md](docs/testing.md) for the full testing guide
(mock backend, test harnesses, D-Bus testing, observability, coverage).

Three test crates, three layers:

- `tisvlay-test` — OS-environment fakes (private `dbus-daemon`, fake
  logind, fake screensaver). Low-level primitives.
- `tisvlay-mock` — fake vendor backend (implements `tisvlay-core`
  traits). Peer-side substitute for a real TV.
- `tisvlay-harness` — binary-lifecycle fixtures (`TestDaemon`,
  `TestSession`, `TestTray`). Composes the other two and reaches into
  the daemon/session internals via each crate's `#[doc(hidden)] pub
  mod __internals` re-export surface. All three are `publish = false`.

Quick reference commands:

```bash
just test                   # cargo nextest run + doc tests
just test-ci                # ci profile + coverage + ratchet
just cov                    # coverage summary
just cov-html               # HTML report in browser
just cov-lcov               # lcov.info for VS Code Coverage Gutters
just cov-ratchet            # fails if below baseline (CI)
just test-snapshot-review   # review pending insta snapshot changes
just test-snapshot-accept   # accept all pending snapshot changes
```

## Reference Documentation

- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
- [docs/testing.md](docs/testing.md) - Testing guide: mock backend, harnesses, coverage, observability
- [docs/watchers/network.md](docs/watchers/network.md) - Network monitoring: carrier detection, route resolution, pitfalls
- [docs/firewall.md](docs/firewall.md) - SSDP firewall configuration
- [docs/bugs.md](docs/bugs.md) - Known bugs and fix plans
- [docs/todo.md](docs/todo.md) - Design concerns and technical debt
