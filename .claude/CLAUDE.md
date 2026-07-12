# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Supports LG WebOS TVs (network, SSAP) and HDMI-CEC (the Linux CEC subsystem, via `linux-cec`), with a vendor- and transport-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU can't tunnel HDMI-CEC, so the CEC path needs a CEC adapter (USB, or a DP→HDMI adapter on an amdgpu/i915 output) — the network path (LG WebOS SSAP) needs none.

Architecture (since v0.3.0): a **bevy_ecs world** holds all daemon state machines, and **zenoh over Unix sockets** is the IPC surface — cached publishers for state, queryables for rpcs, liveliness tokens for presence. The served-D-Bus stack shipped through v0.2.0 and was then removed; consumed desktop D-Bus (logind, ScreenSaver, ksni tray, notifications, `org.freedesktop.Application` GUI activation) remains zbus. The authoritative architecture reference (topology, key space, world/reactor model, empirical zenoh laws) is [docs/architecture.md](../docs/architecture.md); the plan documents under docs/plans/ are the historical phase logs.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## What's Next

P7 remainder (user-assisted): a week of full-stack systemd duty on the real hardware (soaking PR #106 with the post-parity cleanup pass), the G4/suspend/WoL field batch, and the v0.2.0 release PR into main. Follow-ups filed in docs/ideas/: IPC performance harness (zenoh vs D-Bus numbers), tray gettext re-wiring, coverage baseline re-measurement.

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root (one workspace, version 0.3.0)
├── crates/
│   ├── tisvlay-core/              # Core library (vendor-agnostic traits & types)
│   │   └── src/                  # traits, capabilities, pairing, config (profiles),
│   │                             # state, wol, wayland (feature), logging (feature),
│   │                             # Coalescer/ReconcilerMap/RefreshTrigger
│   ├── tisvlay-lg/                # LG WebOS vendor implementation (SSAP over wss)
│   ├── tisvlay-cec/               # HDMI-CEC vendor implementation (linux-cec)
│   ├── tisvlay-ssdp/              # SSDP multicast transport (LG discovery)
│   ├── tisvlay-mock/              # In-memory mock backend (always registered)
│   ├── tisvlay-test/              # OS-environment fakes (private dbus-daemon,
│   │                             # fake logind/ScreenSaver) — dev-only
│   │
│   ├── tisvlay-wire/              # Key-space vocabulary, payload structs (= bevy
│   │                             # components = serde wire payloads), typed WireError,
│   │                             # session bootstrap (peer/router/client configs)
│   ├── tisvlay-world/             # bevy_ecs world: components, systems, reactor loop,
│   │                             # backend bridge (deferred acks via CommandDone)
│   ├── tisvlay-daemon/            # System daemon binary+lib: world + zenoh bridge,
│   │   └── src/                  # store (TOML persistence), seed config, discovery,
│   │                             # per-connector dpms watchers, logind, netlink, arp,
│   │                             # probe (EDID matcher driving the watcher set)
│   ├── tisvlay-session/           # Per-user session service: gateway (client link to
│   │                             # system socket + router owning user/**), user profiles,
│   │                             # ScreenSaver lock/unlock, wayland idle
│   ├── tisvlay-greeter/           # Login-greeter idle client (ext-idle-notify → greeter/idle)
│   ├── tisvlay-tray/              # ksni tray over a live mirror + desktop notifications
│   │                             # (notify.rs: settings service on the user socket)
│   ├── tisvlay/                   # Client SDK: DaemonClient/SessionClient/TrayClient +
│   │                             # StateWatcher (root-D-Bus-client-compatible surface)
│   ├── tisvlay-cli/               # CLI (binary name: tisvlay) — pure zenoh client
│   └── tisvlay-gui/               # Slint settings window (ui/*.slint, lang/ gettext)
│
├── packaging/
│   ├── nix/                      # package.nix, module.nix (services.tisvlay), test.nix (VM)
│   ├── systemd/                  # tisvlay-daemon.service (system), session/tray/greeter
│   │                             # (user units), org.tisvlay.Gui.service (GUI activation)
│   ├── freedesktop/              # org.tisvlay.Gui.desktop
│   ├── licensing/                # cargo-about config + NOTICE
│   └── scripts/                  # nfpm postinstall/preremove
│
├── docs/                         # design.md (RETIRED D-Bus stack, ≤ v0.2.0),
│   ├── architecture.md           # ← current architecture reference
│   ├── plans/                    # historical phase logs (spike, parity)
│   ├── lg/, cec/, watchers/      # vendor + subsystem notes (transport-independent)
│   └── firewall.md, testing.md, ideas.md, bugs.md
└── .claude/CLAUDE.md             # This file
```

## Key Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| IPC | `zenoh`, `zenoh-ext` (advanced pub/sub, `unstable` feature) |
| State machines | `bevy_ecs` |
| WebSocket (LG) | `tokio-tungstenite` + `rustls` |
| JSON / config | `serde`, `serde_json`, `toml`, `directories` |
| CLI | `clap` |
| Consumed D-Bus | `zbus` (logind, ScreenSaver, ksni, GUI activation) |
| System tray | `ksni` |
| Notifications | `notify-rust` (z-with-tokio) |
| Wayland | `wayland-client`, `wayland-protocols` |
| Errors | `thiserror`, `anyhow` |
| Logging | `tracing`, `tracing-subscriber`, `tracing-journald` |
| WoL | `socket2` |
| Netlink | `rtnetlink`, `netlink-packet-route`, `netlink-packet-core` |
| HDMI-CEC | `linux-cec` (LGPL; crates.io) |
| EDID | `edidkit` |

## Known Quirks

Vendor/protocol quirks (unchanged by the rewrite):

- Newer LG firmware requires wss:// (port 3001) with self-signed cert; uses `rustls` with custom `InsecureCertVerifier`
- TV may ignore WoL during pixel refresh; ~1-5 s needed between WoL and first command; need "TV On via Wi-Fi" enabled
- TV closes WebSocket on standby; reconnection logic required
- Backlight is SSAP-subscribable but writes need the Luna `createAlert` workaround (direct setSystemSettings returns 401)
- SSAP subscription updates carry the original request ID, not the URI — routing uses a request-ID→URI map
- LG pairing is two-phase (`Response` prompt shown → `Registered` accepted); see docs/lg/firmware-quirks.md
- CEC: Connected ≠ powered (reachable in standby); `power_on` = Image View On wake-in-place; INPUT capability deliberately not advertised — see docs/cec/protocol.md
- SSDP discovery pins source port 1901 to match the firewall rule (docs/firewall.md)
- KWin on Wayland writes DPMS-off through to the legacy sysfs `dpms` connector attribute, so the daemon polls it without a per-DE compositor protocol; the watcher gates on `status == connected`

Remote-session detection quirks (session-side detectors — see docs/remote/detection.md):

- No first-class "remote session active" API on the Linux desktop — detection is side-effect observation (PipeWire capture node, portal `BecomeMonitor`, logind) plus app hooks. Sunshine's default NvFBC/KMS capture on Nvidia is invisible to the portal monitor *and* PipeWire; its only machine-readable signal is the unauthenticated `GET 127.0.0.1:47989/serverinfo` (`SUNSHINE_SERVER_BUSY|FREE`), polled as a level detector
- Portal `BecomeMonitor` connections are receive-only — resolve a caller's bus name → PID → exe on the *main* session-bus connection, not the monitor connection
- InputCapture portal sessions must never arm (lan-mouse holds a standing one while the user is present); it shares the portal object path + `CreateSession` member with the capture interfaces, so exclude it by interface, not heuristic
- logind mints `Remote=yes` for plain SSH shells too (`Type=tty`); the remote-*desktop* detector must require a graphical `Type` (`x11`/`wayland`)
- NixOS `makeWrapper` truncates process comm to the kernel's 15-byte limit, so Sunshine's comm is `.sunshine-wrapp` (from `.sunshine-wrapped`) — procfs presence gates match `comm.trim_start_matches('.').starts_with("sunshine")`, not `== "sunshine"`

Zenoh stack findings (load-bearing, learned empirically):

- **Consume-only peer leaves**: a connect-only peer (resilient link) can subscribe, query, and hydrate caches, but anything it SERVES (queryables, liveliness tokens, cached pubs) is invisible to other leaves through a hub — under both peer-mode and router-mode hubs. Servers must use client-mode links (fail-fast open, supervised retry loop) into a router hub. The session service's user socket is a router for exactly this reason; the tray's settings service is a supervised client link.
- Zenoh *client* mode keeps a single link (`single_link: true`): a multi-endpoint connect list is failover, not fan-out — hence two sessions per user client (system + user socket), which also makes user/** isolation structural.
- Advanced-cache hydration is subscriber-history-only: the cache queryable sits under a verbatim `@adv` suffix, plain gets can't reach it.
- Multi-reply queries (discovery streaming) need `ConsolidationMode::None` querier-side or consolidation eats same-key replies.
- `timestamping/enabled` must be on daemon-side for cache ordering.
- Zenoh publishers guarantee no cross-key sample ordering — tests wait on combined predicates in a single drain (never two sequential waits on one subscriber).
- A zenoh `get` matching zero queryables completes instantly with no reply — first-rpc-after-serve paths retry the no-reply case.
- Zenoh rejects tokio's current-thread scheduler — zenoh-touching tests need `#[tokio::test(flavor = "multi_thread")]`.
- Stale Unix socket files are fine: zenoh's `.lock` protocol takes over an unclean listener path on the next bind.
- Reply-side backpressure is load-bearing: backend commands ack on TV completion (`BackendEvent::CommandDone`), not on enqueue — this is what makes the tray's Coalescer "latest wins" actually bound queue depth (the C3 volume-storm fix).

Deployment model:

- The daemon chmods its socket 0660 after bind (packaged unit runs UMask=0077); `tisvlay` group membership is the authorization boundary (the old D-Bus policy allowed any local user).
- Connect-side socket defaults fall back to `/run/tisvlay/zenoh.sock` when the dev socket is absent; listen-side defaults never fall back (a dev daemon must not take over the packaged socket).

## Quick Reference

### Running the Stack (dev)

All dev-run targets share socket/site knobs (see the justfile dev section):

```bash
just daemon-start        # system-socket owner; DPMS_CONNECTOR=… EPHEMERAL=true SSDP_PORT=…
just session-start       # user-socket owner (lock/idle dispatch, user profiles)
just tray-start
just gui-start           # GUI_MCP=true for the Slint MCP server
just greeter-start       # needs a wayland compositor
just stop-all
just cli <verb…>         # any CLI verb against the dev sockets, e.g. `just cli list`
```

### CLI smoke tests

```bash
tisvlay status                       # daemon liveliness probe
tisvlay vendors                      # vendor catalog
tisvlay discover                     # SSDP + mock + cec scan
tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.1.50"}'
tisvlay list                         # hydrate from publisher caches
tisvlay watch                        # stream every data key: history, then live
tisvlay set-volume <uuid> 40
tisvlay get-config <uuid>
tisvlay update-config <uuid> '{"name":"Renamed"}'
tisvlay profile list                 # system profiles (editable default included)
tisvlay user-profile list --user-socket …
```

### NixOS Installation

```nix
# flake.nix input
inputs.tisvlay.url = "github:ruffsl/tisvlay";

# configuration.nix
{ inputs, ... }: {
  imports = [ inputs.tisvlay.nixosModules.default ];
  services.tisvlay.enable = true;
  services.tisvlay.users = [ "yourname" ];   # socket-permission grant
}
```

This sets up the `tisvlay-daemon` system service (runs as `tisvlay` user, state in `/var/lib/tisvlay/`, socket in `/run/tisvlay/`), `tisvlay-session` + `tisvlay-tray` user services, the CLI in `$PATH`, and optionally the greeter client (`greeter.enable`).

## Tasks & Commands (`just`)

All project commands live in the `justfile` at the repo root — it is the single source of truth. prek hooks, VSCode tasks, and CI workflows all dispatch through `just` so command strings are defined in exactly one place.

```bash
just              # list all recipes, grouped by purpose
just check        # deny + licenses + fmt + build + clippy + test
just build        # cargo build --profile test --workspace --all-targets --all-features
just clippy       # cargo clippy ... -- -D warnings
just test         # cargo nextest run + cargo test --doc
just test-ci      # nextest with the 'ci' profile (JUnit XML + coverage)
just cov          # coverage summary
just package-deb  # build the .deb package
```

`just` is provided by the Nix dev shell. Outside Nix, install with `cargo install just` or your distro package manager.

## Internationalization (i18n)

`tisvlay-gui` is wired for gettext translation (Slint `@tr()` + Rust `gettext()`); v1 ships English only. The ported tray is not gettext-wired yet (tracked in docs/ideas.md). Architecture and workflow: [docs/i18n.md](../docs/i18n.md). After adding or changing user-facing GUI strings, run `just i18n-extract` and commit the refreshed `.pot` diff.

- **Translate**: button labels, pane headers, status text, menu items, tooltip copy, error messages.
- **Don't translate**: brand names (`"TisVlay"`, `"LG WebOS"`), machine identifiers (`"awaiting_tv"`, `"lg"`), Unicode glyphs, technical placeholders, file paths, URLs.
- **Format strings**: use `gettext("Foo: {0}").replace("{0}", &v.to_string())` — never `format!`, which `xgettext` can't extract from.

## Linting & CI

All checks are defined in `prek.toml`, which dispatches to `just` recipes. Hook ids match the corresponding `just` recipe names, ordered by priority: file checks → `fmt` → `build` → `clippy` → `test`. Build and clippy use `--profile test` so `cargo test` reuses all build artifacts.

```bash
prek run -a                                          # all checks
prek run -a --skip build --skip clippy --skip test   # fast file checks + fmt only
```

CI runs file checks via prek, then a single `just test-ci` (clippy + nextest + doc tests + coverage under one instrumented build, `--all-features`). Design notes: `docs/build.md`.

### Dead Code Detection

Public items are invisible to Rust's `dead_code` lint. Use `cargo workspace-unused-pub` periodically after major refactors; false positives include trait impl methods required by frameworks (rustls, ksni, zenoh handlers) and builder methods reserved for future use.

### Testing

See [docs/testing.md](../docs/testing.md) for the testing guide. Layers:

- `tisvlay-test` — OS-environment fakes (private `dbus-daemon`, fake logind, fake ScreenSaver) for the consumed-D-Bus adapters.
- `tisvlay-mock` — fake vendor backend (implements `tisvlay-core` traits), always registered in the daemon.
- Wire-level integration tests live in each binary crate's `tests/` (daemon: wire/profiles/cec/greeter/logind/liveliness; session; tray mirror + settings; SDK client) — they boot real daemon/session stacks on temp sockets, no fixture crate needed.

```bash
just test                   # cargo nextest run + doc tests
just cov / cov-html / cov-ratchet
just test-snapshot-review   # insta
```

## Reference Documentation

- [docs/architecture.md](../docs/architecture.md) - **Current architecture**: topology, key space, world/reactor model, deployment, empirical zenoh laws
- [docs/plans/parity-zenoh-ecs.md](../docs/plans/parity-zenoh-ecs.md) - The rewrite's phase log and war stories (historical)
- [docs/design.md](../docs/design.md) - The retired served-D-Bus architecture (≤ v0.2.0); kept for history
- [docs/discovery.md](../docs/discovery.md) - SSDP transport, streaming discovery, manual-entry MAC enrichment
- [docs/i18n.md](../docs/i18n.md) - Translation architecture and workflow
- [docs/testing.md](../docs/testing.md) - Testing guide
- [docs/lg/firmware-quirks.md](../docs/lg/firmware-quirks.md) - LG WebOS firmware quirks
- [docs/cec/protocol.md](../docs/cec/protocol.md) - HDMI-CEC backend model
- [docs/cec/testing.md](../docs/cec/testing.md) - CecLink seam + FakeCecLink
- [docs/watchers/network.md](../docs/watchers/network.md) - Carrier detection, route resolution
- [docs/firewall.md](../docs/firewall.md) - SSDP firewall configuration
- [docs/ideas.md](../docs/ideas.md) - Deferred work + research notes
