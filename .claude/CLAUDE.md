# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Supports LG WebOS TVs (network, SSAP) and HDMI-CEC (the Linux CEC subsystem, via `linux-cec`), with a vendor- and transport-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU can't tunnel HDMI-CEC, so the CEC path needs a CEC adapter (USB, or a DP→HDMI adapter on an amdgpu/i915 output) — the network path (LG WebOS SSAP) needs none.

Architecture (since v0.3.0): a **bevy_ecs world** holds all daemon state machines, and **zenoh over Unix sockets** is the IPC surface — cached publishers for state, queryables for rpcs, liveliness tokens for presence. The served-D-Bus stack shipped through v0.2.0 and was then removed; consumed desktop D-Bus (logind, Mutter's IdleMonitor, xdg-desktop-portal, the file manager, ksni tray, notifications, `org.freedesktop.Application` GUI activation) remains zbus. The authoritative architecture reference (topology, key space, world/reactor model, empirical zenoh laws) is [docs/architecture.md](../docs/architecture.md); the rewrite's phase-by-phase history lives in git.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## What's Next

- **Release automation after v0.3.0.** v0.3.0 shipped 2026-10-14: the repo is public at TisVlay/TisVlay, and the three `publish = true` crates (core, protocol and the `tisvlay` SDK) were published to crates.io by hand. The vendor backends (lg, cec) and ssdp stay unpublished, since only the unpublished daemon uses them. Still to set up: each crate's trusted publisher plus the `crates-io` environment (docs/build.md § First-publish checklist, step 7); a `maintainers` org team as crates.io co-owner; then "Allow GitHub Actions to create and approve pull requests" and the release-plz workflow, which is disabled in GitHub's settings until then (`gh workflow enable release-plz.yml`).
- **Follow-ups:** docs/ideas/switchover-followups.md (IPC performance harness, tray gettext), docs/ideas/screencast-detection-coverage.md (no desktop-agnostic signal exists; options for GNOME and others).

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root (one workspace, version 0.5.0)
├── crates/
│   ├── tisvlay-core/              # Core library (vendor-agnostic traits & types)
│   │   └── src/                  # traits, capabilities, pairing, config (profiles),
│   │                             # state, vendor config, secrets, wol
│   ├── tisvlay-runtime/           # Binaries' process glue (unpublished): logging init,
│   │                             # socket-group access, wayland + Mutter idle transports,
│   │                             # durable state-file writes, GUI/tray client helpers
│   │                             # (Coalescer/ReconcilerMap, profile refs)
│   ├── tisvlay-lg/                # LG WebOS vendor implementation (SSAP over wss)
│   ├── tisvlay-cec/               # HDMI-CEC vendor implementation (linux-cec)
│   ├── tisvlay-ssdp/              # SSDP multicast transport (LG discovery)
│   ├── tisvlay-mock/              # In-memory mock backend (always registered, unpublished)
│   ├── tisvlay-icons/             # freedesktop icon-theme resolution (GUI and tray)
│   ├── tisvlay-test/              # OS-environment fakes (private dbus-daemon,
│   │                             # fake logind, link-breaking relay) — dev-only
│   │
│   ├── tisvlay-protocol/          # Key-space vocabulary, payload structs (serde wire
│   │                             # payloads; the world stores them as State<T>), typed
│   │                             # WireError, session bootstrap (peer/router/client configs)
│   ├── tisvlay-world/             # bevy_ecs world: components, systems, reactor loop,
│   │                             # backend bridge (deferred acks via CommandDone)
│   ├── tisvlay-daemon/            # System daemon lib: world + zenoh bridge,
│   │   └── src/                  # store (TOML persistence), seed config, discovery,
│   │                             # per-connector dpms watchers, logind, netlink, arp,
│   │                             # probe (EDID matcher driving the watcher set)
│   ├── tisvlay-session/           # Per-user session service: gateway (client link to
│   │                             # system socket + router owning user/**), user profiles,
│   │                             # logind lock/unlock, wayland and Mutter idle
│   ├── tisvlay-greeter/           # Login-greeter idle client (ext-idle-notify → greeter/idle)
│   ├── tisvlay-tray/              # ksni tray over the SDK clients + desktop notifications
│   │                             # (notify.rs: settings service on the user socket)
│   ├── tisvlay/                   # Client SDK: DaemonClient/SessionClient/TrayClient +
│   │                             # StateWatcher
│   ├── tisvlay-cli/               # The `tisvlay` binary: zenoh CLI client + the
│   │                             # `service` verbs (daemon/session/tray/greeter);
│   │                             # its lib exposes the clap tree for docgen
│   ├── tisvlay-docgen/            # gen-docs: renders the mdBook's generated partials
│   │                             # (CLI reference, profile defaults, release version)
│   ├── tisvlay-gui/               # Compiled Slint UI only (ui/*.slint, lang/ gettext;
│   │                             # owns the gettext domain; lib name tisvlay_gui_ui)
│   └── tisvlay-gui-app/           # Settings window: state, rpcs, tests, `tisvlay-gui`
│                                 # binary (lib name tisvlay_gui)
│
├── .github/                      # CONTRIBUTING.md, SECURITY.md, PR + issue templates,
│                                 # workflows, dependabot.yml
├── packaging/
│   ├── nix/                      # package.nix, module.nix (services.tisvlay), test.nix (VM)
│   ├── systemd/                  # tisvlay-daemon.service (system), session/tray/greeter
│   │                             # (user units), org.tisvlay.Gui.service (GUI activation)
│   ├── freedesktop/              # org.tisvlay.Gui.desktop
│   ├── licensing/                # cargo-about config + NOTICE, deny.toml
│   ├── completions/, polkit/,    # shell completions (generated), the sleep rule, the TV remote's
│   │   udev/                     # keymap
│   ├── docker/                   # CI-like build environment, without Nix
│   └── scripts/                  # nfpm postinstall/preremove
│
├── docs/
│   ├── architecture.md           # ← current architecture reference
│   ├── build.md                  # build, lint policy, public API policy, first-publish checklist
│   ├── conventions.md            # code conventions (imported below)
│   ├── book/                     # mdBook user guide (Read the Docs); src/generated/ is docgen output
│   ├── plans/                    # in-flight plan docs (README convention; git holds retired ones)
│   ├── backends/                 # implementing a vendor backend
│   ├── lg/, cec/, watchers/,     # vendor + subsystem notes (transport-independent)
│   │   streams/                  # stream detectors
│   ├── ideas/, bugs/, concerns/  # one doc per item; each folder's README.md is its index
│   └── firewall.md, testing.md
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
| Consumed D-Bus | `zbus` (logind, Mutter idle, portals, file manager, ksni, GUI activation) |
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
- TV may ignore WoL during pixel refresh; it takes several seconds after WoL to accept commands, longer from deep standby (measured in docs/lg/benchmarks/wake-reconnect.md); need "TV On via Wi-Fi" enabled
- TV closes WebSocket on standby; reconnection logic required
- Backlight is SSAP-subscribable but writes need the Luna `createAlert` workaround (direct setSystemSettings returns 401)
- SSAP subscription updates carry the original request ID, not the URI — routing uses a request-ID→URI map
- LG pairing is two-phase (`Response` prompt shown → `Registered` accepted); see docs/lg/firmware-quirks.md
- CEC: Connected ≠ powered (reachable in standby); `power_on` = Image View On wake-in-place; INPUT capability deliberately not advertised — see docs/cec/protocol.md
- CEC: the kernel keeps a logical-address claim after the handle closes, and clearing it blanks the OSD name — so the connection-holding opens clear before setting the name, and the probe/bare wake reuse a held claim (`link::Claim`)
- SSDP discovery pins source port 1901 so one firewall rule admits the replies (why: docs/firewall.md; the rules: the book's Firewall & Discovery)
- KWin on Wayland writes DPMS-off through to the legacy sysfs `dpms` connector attribute, so the daemon polls it without a per-DE compositor protocol; the watcher gates on `status == connected`

Stream detection quirks (session-side detectors — see docs/streams/detection.md):

- No first-class "stream active" API on the Linux desktop — detection is side-effect observation (PipeWire capture node, portal `BecomeMonitor`, logind) plus app hooks. Sunshine's default NvFBC/KMS capture on Nvidia is invisible to the portal monitor *and* PipeWire; its only machine-readable signal is the unauthenticated `GET 127.0.0.1:<base port>/serverinfo` (`SUNSHINE_SERVER_BUSY|FREE`; base port 47989 by default, `--sunshine-port` otherwise — a Sunshine moved aside for Moonshine leaves Moonshine answering on 47989), polled as a level detector
- Portal `BecomeMonitor` connections are receive-only — resolve a caller's bus name → PID → exe on the *main* session-bus connection, not the monitor connection
- InputCapture portal sessions must never arm (lan-mouse holds a standing one while the user is present); it shares the portal object path + `CreateSession` member with the capture interfaces, so exclude it by interface, not heuristic
- logind mints `Remote=yes` for plain SSH shells too (`Type=tty`); the remote-*desktop* detector must require a graphical `Type` (`x11`/`wayland`)
- `pipewire-native` (the screencast detector's client) loads PipeWire's C support plugin from an SPA plugin dir baked at build time, so Nix builds need pipewire as a build input; the session checks the plugin exists before starting the detector and skips `client.conf` (`config.name = "null"`, avoiding the `spa-json-dump` tool). KWin capture nodes are matched by `media.name` prefix `kwin-screencast-`, never `node.name` (the process binary, renamed by wrappers), and `media.name` isn't in the registry's global props, so candidates are bound and judged on their full info — see docs/streams/detection.md
- NixOS `makeWrapper` truncates process comm to the kernel's 15-byte limit, so Sunshine's comm is `.sunshine-wrapp` (from `.sunshine-wrapped`) — procfs presence gates match `comm.trim_start_matches('.').starts_with("sunshine")`, not `== "sunshine"`

Zenoh stack findings: the full list, with the reasons, is [docs/architecture.md](../docs/architecture.md) § Empirical zenoh laws. Violating one produces silent wrong behavior, so read it before touching links, caches or rpcs. The ones code changes trip most:

- A connect-only peer can consume but can't serve to *other* leaves through a hub; servers use client-mode links into a router (architecture.md § Link modes and the serve-side law).
- A zenoh client keeps a single link, so each user client opens two sessions, one per socket.
- Publishers guarantee no cross-key ordering, and history replay races live samples: tests wait on combined predicates in one drain.
- A `get` matching zero queryables returns no reply at once: clients gate rpcs on querier matching, not on liveliness.
- Zenoh-touching tests need `#[tokio::test(flavor = "multi_thread")]`.
- Backend commands ack on TV completion (`BackendEvent::CommandDone`), not on enqueue; the tray's Coalescer relies on it.

Deployment model:

- Socket access is layered: `tisvlay` group membership, plus daemon-applied ACLs for the greeter group and each seat's active graphical user. Canonical: [packaging/README.md](../packaging/README.md) § "Socket access & the ACL integration".
- With no `--socket`, clients use the per-user dev socket when it accepts a connection, else `/run/tisvlay/zenoh.sock` if it exists, else the dev path (and wait for a dev daemon); listen-side defaults never fall back (a dev daemon must not take over the packaged socket).
- Shell completions for the `tisvlay` CLI (bash/zsh/fish) ship in every package and the nix build. They use clap_complete's dynamic engine, so `<TAB>` completes live TV UUIDs by calling back into the binary; generation and per-distro caveats are in [packaging/README.md](../packaging/README.md).

## Quick Reference

### Running the Stack (dev)

The contributing guide's Setup has the basic `*-start` recipes, `just cli` and `just stop-all`. Beyond those, the dev-run targets share socket and site knobs (the justfile's dev section): `DPMS_CONNECTOR=…`, `EPHEMERAL=true` and `SSDP_PORT=…` for the daemon, `GUI_MCP=true` for the Slint MCP server, and `just greeter-start`, which needs a Wayland compositor.

After a `flake.nix` change, run `direnv reload` (or reopen the terminal) before restarting the stack: a stale dev shell rebuilds with the old inputs, e.g. the session without pipewire's pkg-config entry, so its screencast detector stays off.

### CLI smoke tests

```bash
tisvlay status                       # daemon liveliness + release compatibility
tisvlay vendor                       # vendor catalog (`vendor lg`: pairing fields)
tisvlay discover                     # SSDP + mock + cec scan (--watch: until ctrl-c)
tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.1.50"}'
tisvlay list                         # hydrate from publisher caches
tisvlay watch                        # stream every data key: history, then live
tisvlay volume <uuid> 40             # set; `tisvlay volume <uuid>` reads it back
tisvlay mute <uuid> --toggle
tisvlay config <uuid> '{"name":"Renamed"}'
tisvlay profile system list          # system profiles (editable default included)
tisvlay profile user list --user-socket …
```

### NixOS Installation

```nix
# flake.nix input
inputs.tisvlay.url = "github:TisVlay/TisVlay";

# configuration.nix
{ inputs, ... }: {
  imports = [ inputs.tisvlay.nixosModules.default ];
  services.tisvlay.enable = true;
  services.tisvlay.users = [ "yourname" ];   # socket-permission grant
}
```

This sets up the `tisvlay-daemon` system service (runs as `tisvlay` user, state in `/var/lib/tisvlay/`, socket in `/run/tisvlay/`), `tisvlay-session` + `tisvlay-tray` user services, the CLI in `$PATH`, and optionally the greeter client (`greeter.enable`).

## Tasks & Commands (`just`)

All project commands live in the `justfile` at the repo root — it is the single source of truth. prek hooks, VSCode tasks, and CI workflows all dispatch through `just` so command strings are defined in exactly one place. Run `just` to list every recipe, grouped by purpose; `just check` runs the local gates, and `just msrv`, `just hack` and `just audit` reproduce the CI-only ones.

## Internationalization (i18n)

`tisvlay-gui` is wired for gettext translation (Slint `@tr()` + Rust `gettext()`); the tray isn't yet. What to translate and the workflow: [docs/i18n.md](../docs/i18n.md). After adding or changing user-facing GUI strings, run `just i18n-extract` and commit the refreshed `.pot` diff. Format strings use `gettext("Foo: {0}").replace("{0}", &v.to_string())`, never `format!`, which `xgettext` can't extract from.

## Contributing

[.github/CONTRIBUTING.md](../.github/CONTRIBUTING.md) is written for outside contributors. Read § How changes land when asked to land work; the maintainer's instructions win over it. Read [docs/book/README.md](../docs/book/README.md) before editing the book.

@../docs/conventions.md

## Linting & CI

All checks are defined in `prek.toml`, which dispatches to `just` recipes. Hook ids match the corresponding `just` recipe names, ordered by priority: file checks → `fmt` → `build` → `clippy`/`doc` → `test`. Build and clippy use `--profile test` so `cargo test` reuses all build artifacts. The gate recipes deny warnings by exporting `CARGO_BUILD_WARNINGS=deny` — never set in `.cargo/config.toml`, which the Nix package build reads; the policy lives in `docs/build.md` § Lint policy.

```bash
prek run -a                                                     # all checks
prek run -a --skip build --skip clippy --skip test --skip doc   # docs-only changes
```

CI runs jobs by what a change touches, and drafts run only the fast checks: `docs/build.md` § What runs when.

### Dead Code Detection

Public items are invisible to Rust's `dead_code` lint. Use `cargo workspace-unused-pub` periodically after major refactors; false positives include trait impl methods required by frameworks (rustls, ksni, zenoh handlers) and builder methods reserved for future use.

### Testing

[docs/testing.md](../docs/testing.md) is the testing guide: the layers (`tisvlay-test`'s OS fakes and link-breaking `Relay`, the `tisvlay-mock` backend, wire tests in each binary crate's `tests/`), the suites, and where new tests go. One integration-test binary per crate; zenoh-touching tests boot real stacks on temp sockets.

```bash
just test                   # cargo nextest run + doc tests
just cov / cov-html / cov-ratchet
```

## Reference Documentation

The contributing guide's § Where to read more lists the main developer docs (architecture, build, testing, and the ideas, concerns and bugs indexes). The subsystem and vendor docs:

- [docs/streams/detection.md](../docs/streams/detection.md) - Stream detectors (PipeWire, portal, logind, Sunshine) and their quirks
- [docs/discovery.md](../docs/discovery.md) - SSDP transport, streaming discovery, manual-entry MAC enrichment
- [docs/firewall.md](../docs/firewall.md) - Why SSDP discovery needs a firewall rule
- [docs/i18n.md](../docs/i18n.md) - Translation architecture and workflow
- [docs/lg/firmware-quirks.md](../docs/lg/firmware-quirks.md) - LG WebOS firmware quirks
- [docs/cec/protocol.md](../docs/cec/protocol.md) - HDMI-CEC backend model
- [docs/cec/testing.md](../docs/cec/testing.md) - CecLink seam + FakeCecLink
- [docs/watchers/network.md](../docs/watchers/network.md) - Carrier detection, route resolution
