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

The TVs and Profiles panes of `tisvlay-gui` are feature-complete (master/detail with live-state controls + edit-mode pencil for TVs; master/detail with create/rename/delete/duplicate + per-field inheritance editor for Profiles; pairing wizard with discovery streaming; About pane). Remaining GUI work, packaging, and architectural follow-ups:

- **GUI polish** — error toasts, keyboard nav, window-state persistence.
- **GUI packaging** — NixOS module `enableGui` flag, nfpm desktop entry, XDG `.desktop` file.
- **Event-driven power-management redesign** — replace today's session-bus listener path with per-DE adapters (KWin / wlroots / X11 / logind fallback). See [docs/ideas.md](../docs/ideas.md). A narrow first slice shipped: the daemon-side DPMS connector watcher (`tisvlayd/src/dpms.rs`) driving the `on_dpms_off`/`on_dpms_on` system-profile actions (default `screen_off`/`screen_on`).
- **Plus other deferred ideas**: polkit gating for multi-user workstations, CLI shorthand for `Tv1.UpdateConfig`, `a{sv}` wire-format migration for vendor configs, atomic config writes, typed `Failed { reason }` payload, "Open logs" affordance. All filed in [docs/ideas.md](../docs/ideas.md).

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
│   │       ├── wayland.rs        # Policy-free ext-idle-notify-v1 transport (feature-gated; session + greeter)
│   │       └── error.rs          # Error types
│   │
│   ├── tisvlay-lg/                # LG WebOS vendor implementation
│   │   └── src/
│   │       ├── lib.rs            # Public exports
│   │       ├── backend.rs        # LgBackend (TvBackend metadata)
│   │       ├── connector.rs      # LgConnector (factory, wake, pairing)
│   │       ├── connection.rs     # LgConnection (TvConnection) + SSAP state parsing
│   │       ├── client.rs         # WebOsClient - high-level SSAP API
│   │       ├── transport.rs      # Raw WebSocket (rustls TLS) + request/response multiplexing
│   │       ├── protocol.rs       # SSAP message types
│   │       ├── endpoints.rs      # API endpoint constants
│   │       ├── config.rs         # LgVendorConfig
│   │       ├── discovery.rs      # LgDiscovery (TvDiscovery) + SSDP search (feature-gated)
│   │       └── test_server.rs    # FakeSsapServer (#[cfg(test)])
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
│   ├── tisvlay/                   # Client SDK (re-exports core's D-Bus client)
│   │   └── src/
│   │       └── lib.rs            # DaemonClient + StateWatcher + state-type re-exports
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
│   │       ├── desktop_probe.rs  # DRM connector enumeration + EDID-based TV ↔ connector matching
│   │       ├── dpms.rs           # DPMS connector watcher → on_dpms_off/on_dpms_on system events
│   │       ├── json_toml.rs      # Lossless JSON ↔ TOML conversion for vendor-config payloads
│   │       ├── registry.rs       # Re-exports from tisvlay-backends
│   │       └── tv_manager.rs     # TV connection pool, profile dispatch, disk serialization
│   │
│   ├── tisvlay-session/           # User session service (lib + bin)
│   │   └── src/
│   │       ├── lib.rs            # Library root (SessionState, event_handler, reconcile_user_config_orphans, __internals)
│   │       ├── main.rs           # Entry point (thin, delegates to lib)
│   │       ├── session_dbus.rs   # org.tisvlay.Session1 D-Bus interface
│   │       ├── session_monitor.rs # Session lock/unlock detection
│   │       ├── wayland_idle.rs   # Greeter-wake policy over tisvlay-core's shared wayland idle transport
│   │       └── idle.rs           # Idle timeout detection
│   │
│   ├── tisvlay-greeter/           # Login-greeter idle client (lib + bin)
│   │   └── src/
│   │       ├── lib.rs            # run() loop: ext-idle-notify-v1 → Manager1.NotifyGreeterIdle; next_notify dedup
│   │       └── main.rs           # Entry point (thin, delegates to lib)
│   │
│   ├── tisvlay-tray/              # System tray app
│   │   └── src/
│   │       ├── main.rs           # Entry point, signal watcher, command processor
│   │       ├── tray.rs           # ksni Tray impl, menu building, TrayCommand
│   │       └── state.rs          # CachedTvState, fetch_all_state()
│   │
│   └── tisvlay-gui/               # Slint-based settings window
│       ├── src/
│       │   ├── lib.rs            # Library root: run() entry, AppWindow construction, all Slint callback wiring
│       │   ├── main.rs           # Thin binary entry, delegates to lib::run
│       │   ├── state.rs          # TvViewRaw, ProfileRowRaw, fetch_state (daemon → Slint snapshot)
│       │   ├── state_watch.rs    # StateEvent → Slint signal-driven refresh dispatcher
│       │   ├── profiles.rs       # Profile editor — ProfileLayers cache, dispatch helpers, signal handlers
│       │   ├── schema_form.rs    # Schema-driven form helpers (validate, autofill, prefill)
│       │   └── dbus.rs           # Single-instance D-Bus claim + Activate handler
│       ├── ui/
│       │   ├── main.slint        # Window root + sidebar nav, AppWindow component
│       │   ├── theme.slint       # Theme tokens global (dark/light)
│       │   ├── design.slint      # Design tokens (gaps, font sizes, radii)
│       │   ├── widgets.slint     # Bespoke primitives (Chip, Group, IconBtn, SpinnerArc, …)
│       │   ├── components/       # Pane components (tvs_pane, profiles_pane, schema_form, …)
│       │   └── globals/          # Cross-pane state globals (Daemon, EditPane, PairingWizard, …)
│       └── lang/                 # gettext message catalogs (.pot template + per-locale .po/.mo)
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
│   ├── discovery.md              # SSDP transport, streaming RPCs, manual-entry MAC enrichment
│   ├── ideas.md                  # Deferred work + research notes
│   ├── concerns.md               # Design concerns / technical debt
│   ├── bugs.md                   # Known bugs index (per-bug docs in bugs/)
│   ├── testing.md                # Testing guide
│   ├── firewall.md               # SSDP firewall configuration
│   ├── configs/                  # Sample daemon + session TOML configs
│   ├── watchers/
│   │   └── network.md            # Network monitoring: carrier, route resolution, pitfalls
│   ├── lg/                       # LG-specific docs (WebOS notes, benchmarks)
│   ├── kde/                      # KDE-specific notes (tray quirks)
│   └── bugs/                     # Per-bug detail docs
│
└── .claude/
    ├── CLAUDE.md                 # This file
    └── slint.md                  # Slint dev workflow + MCP introspection notes
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
- KWin on Wayland writes its DPMS-off through to the legacy sysfs `dpms` connector attribute (`/sys/class/drm/<card>-<conn>/dpms`), so `tisvlayd/src/dpms.rs` can poll it without a per-DE compositor protocol. The `dpms` watcher gates on `status == connected` (a powered-off TV that drops its EDID link reads `disconnected` and is skipped) and defers dispatch while a TV is `reconnecting`. `on_dpms_off`/`on_dpms_on` default to `screen_off`/`screen_on`, so the mirror is active out of the box; when a user session is also running, its idle/lock path can drive the same screen-off/on — a redundant-but-safe double-dispatch (`screen_off` is idempotent; the `reconnecting` gate covers `screen_on`/WoL)
- zbus 5.x's `ObjectServer::at` returns `Ok(false)` (not `Err`) when an interface is already registered at the path. `register_tv_interfaces` uses `.await?` on each of its seven `at()` calls and discards the bool, so a re-register against a stale path *silently fails to refresh state* — the pre-existing interface keeps serving with its old field values. The defensive `unregister_tv_interfaces` before every `register_tv_interfaces` call site (`apply_reload_diff`, `Manager1.AddTv`) is what makes fresh state actually take effect. Foundation contract is locked down by `crates/tisvlayd/tests/register_tv_interfaces_collision.rs`. Future zbus upgrades that change `at()`'s return type are a watch item.
- Per-TV `Mutex<ManagedTv>` is the de-facto drain layer between `Tv1.Remove` (and `apply_reload_diff`'s removed half) and concurrent in-flight `Tv1.*` calls. zbus has no drain primitive; the observable correctness rests on the Mutex-held-across-await pattern in `with_connection` and similar helpers. Three sub-cases (call hasn't reached `get_tv` → `TvNotFound`; call holds Arc awaiting inner Mutex → `NotConnected` after removal closes; call holds Mutex during work → removal blocks until in-flight completes) documented at `tv_manager::remove_tv`. Locked down by `crates/tisvlayd/tests/in_flight_call_during_remove.rs` (invariant-based, not per-case).
- `pair_tv` generates the new TV's UUID upfront (before `start_pairing`) so `Manager1.PairingProgress` signals can carry a stable identifier from the very first phase update — clients watching progress get correlation before any persistent state lands. Same UUID lets `Manager1.CancelPair(tv_id)` reach the in-flight session via the `pairing_sessions` map for both AddTv and re-pair flows.
- `Manager1.AddTv` and `Tv1.RePair` are **async-spawn** (return immediately after sync validation; handshake runs in a daemon-side task). Outcome surfaces via `Manager1.PairingProgress(tv_id, phase, message)` — clients filter on `tv_id` and drain to terminal `complete` / `failed` via `DaemonClient::drain_pair_to_terminal`. The daemon defers the terminal `complete` emit until *after* the per-TV `Tv1.*` interfaces are registered, so clients seeing `complete` can immediately call into the new TV without racing `ObjectManager.InterfacesAdded`. `failed` comes from any post-handshake setup failure (post-handshake connect, interface register) as well as the handshake itself; `run_pairing_handshake`'s internal progress task only emits non-terminal phases now (the outer `run_pair_task` / `run_re_pair_task` owns the terminal emit).
- `PairingSessionGuard` (RAII) removes `pairing_sessions` entries on every exit path of `run_pairing_handshake` — Ok, Err, panic-unwind, future-dropped. Manual cleanup would silently leak entries if the spawned task was aborted mid-handshake.
- The GUI wizard tracks a `WizardGen = Arc<AtomicU64>` generation counter, bumped by `reset_wizard`. Each `run_wizard_pair` / `run_wizard_re_pair` task captures the value at start; every `on_ui` callback and `publish_pair_outcome` no-ops if the live counter has drifted. Protects against a user clicking Cancel + reopening the wizard before the prior task's terminal phase arrives.

## Quick Reference

### Running the Stack

```bash
# Development (session bus):
tisvlayd --session
tisvlay-session --user
tisvlay-tray --user
tisvlay-gui --user      # optional — settings window

# Production (system bus, as systemd services):
sudo tisvlayd
systemctl --user enable --now tisvlay-session
systemctl --user enable --now tisvlay-tray
tisvlay-gui              # launched on demand from app menu
```

### Testing with busctl

Per-TV object paths use underscore-encoded UUIDs since the Phase 8.1 identity migration. The examples below use `<UUID_>` as a stand-in — replace with the actual path component (run `busctl --user tree org.tisvlay.Daemon` or `tisvlay list` to see).

```bash
# Manager interface
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetPairingSchema s lg
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ListSystemProfiles

# List all objects + properties (single round-trip for all TV state)
busctl --user call org.tisvlay.Daemon /org/tisvlay org.freedesktop.DBus.ObjectManager GetManagedObjects

# Per-TV identity + cached state
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_>
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 Connected
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 VendorConfig
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 VendorInfo

# Single-field config edits via the per-property setters
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 Name s "Living Room G4"
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 Managed b true
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 SystemProfile s default
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 PrimaryInput s HDMI_1

# Multi-field atomic edit via Tv1.UpdateConfig
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 UpdateConfig s '{"name":"Renamed","mac":"AA:BB:CC:DD:EE:FF"}'

# Read current config (top-level + non-Secret vendor fields, JSON-string)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 GetConfig

# Lifecycle
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 Reconnect
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1 Remove

# Writable live-state methods (properties stay read-only; Set* methods are the auth path)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Volume SetVolume y 30
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Volume SetMuted b true
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Screen SetScreen b false
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Backlight SetBacklight y 80

# Other methods
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Power PowerOff
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Power PowerOn
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/<UUID_> org.tisvlay.Tv1.Input SwitchInput s HDMI_2

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

## Internationalization (i18n)

Both `tisvlay-gui` and `tisvlay-tray` are wired for gettext translation; v1 ships English only. Architecture, workflow, "add a new locale" walkthrough, and decision rationale (why gettext over Fluent / i18n-embed / rust-i18n, why a build.rs vs checked-in `.mo`, why `bindtextdomain` over `gettextrs::TextDomain`, the Slint external-translator escape hatch) live in [docs/i18n.md](../docs/i18n.md).

Quick reference for new strings — what to translate:

- **Translate**: button labels, pane headers, status text, menu items, tooltip copy, helper copy, error messages.
- **Don't translate**: brand names (`"TisVlay"`, `"LG WebOS"`), machine identifiers (`"awaiting_tv"`, `"lg"`, `"system-shutdown"`), Unicode glyphs (`"●"`, `"·"`, `"—"`, `"↻"`, `"►"`), technical placeholders (`"192.168.1.100"`, `"AA:BB:CC:DD:EE:FF"`), font-family CSS values, file paths, URLs.
- **Brand inside composite templates**: wrap the whole template — translators pass `"TisVlay"` through verbatim.
- **Format strings**: use `gettext("Foo: {0}").replace("{0}", &v.to_string())` — never `format!("Foo: {}", v)`, which `xgettext` can't extract from.

After adding or changing user-facing strings, run `just i18n-extract` and commit the refreshed `.pot` diff.

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

# CI runs file checks via prek, then a single `just test-ci` that
# wraps clippy + nextest + doc tests + coverage report under one
# instrumented build (`cargo llvm-cov show-env`). Mock-only feature
# subset for CI lives in `justfile` as `ci_features`; rationale and
# extension triggers are in its comment block. Design notes:
# `docs/build.md`.
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
- [docs/discovery.md](docs/discovery.md) - SSDP transport ownership, streaming-discovery RPCs, manual-entry MAC enrichment, schema-form trust signal
- [docs/i18n.md](docs/i18n.md) - Translation architecture, workflow, add-a-locale walkthrough, decision rationale (gettext-rs vs Fluent / i18n-embed)
- [docs/testing.md](docs/testing.md) - Testing guide: mock backend, harnesses, coverage, observability
- [docs/lg/firmware-quirks.md](docs/lg/firmware-quirks.md) - LG WebOS firmware quirks (Bug A/B, `processing`/`onOff` discriminators, post-wake settle, `REQUEST_MS`-is-load-bearing)
- [docs/watchers/network.md](docs/watchers/network.md) - Network monitoring: carrier detection, route resolution, pitfalls
- [docs/firewall.md](docs/firewall.md) - SSDP firewall configuration
- [docs/bugs.md](docs/bugs.md) - Known bugs and fix plans
- [docs/todo.md](docs/todo.md) - Design concerns and technical debt
