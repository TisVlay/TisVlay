# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Supports LG WebOS TVs (network, SSAP) and HDMI-CEC (the Linux CEC subsystem, via `linux-cec`), with a vendor- and transport-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU can't tunnel HDMI-CEC, so the CEC path needs a CEC adapter (USB, or a DP→HDMI adapter on an amdgpu/i915 output) — the network path (LG WebOS SSAP) needs none.

Architecture (since v0.3.0): a **bevy_ecs world** holds all daemon state machines, and **zenoh over Unix sockets** is the IPC surface — cached publishers for state, queryables for rpcs, liveliness tokens for presence. The served-D-Bus stack shipped through v0.2.0 and was then removed; consumed desktop D-Bus (logind, ScreenSaver, ksni tray, notifications, `org.freedesktop.Application` GUI activation) remains zbus. The authoritative architecture reference (topology, key space, world/reactor model, empirical zenoh laws) is [docs/architecture.md](../docs/architecture.md); the rewrite's phase-by-phase history lives in git.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## What's Next

- **Release automation after v0.3.0.** v0.3.0 shipped 2026-10-14: the repo is public at TisVlay/TisVlay, and the three `publish = true` crates (core, protocol and the `tisvlay` SDK) were published to crates.io by hand. The vendor backends (lg, cec) and ssdp stay unpublished, since only the unpublished daemon uses them. Still to set up: each crate's trusted publisher plus the `crates-io` environment (docs/build.md § First-publish checklist, step 7); a `maintainers` org team as crates.io co-owner; then "Allow GitHub Actions to create and approve pull requests" and the release-plz workflow, which is disabled in GitHub's settings until then (`gh workflow enable release-plz.yml`).
- **User docs:** the mdBook guide isn't on Read the Docs yet: import the public repo and set the project's "Path for .readthedocs.yaml" to `.config/.readthedocs.yaml`. The fragment-aware `docs-links` gate is in CI; after connecting Read the Docs, retire docs/plans/mdbook-docs-site.md.
- **Follow-ups:** docs/ideas/switchover-followups.md (IPC performance harness, tray gettext, coverage baseline), docs/ideas/screencast-detection-coverage.md (no desktop-agnostic signal exists; options for GNOME and others).

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root (one workspace, version 0.4.0)
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
│   ├── tisvlay-test/              # OS-environment fakes (private dbus-daemon,
│   │                             # fake logind/ScreenSaver, link-breaking relay) — dev-only
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
│   │                             # ScreenSaver lock/unlock, wayland idle
│   ├── tisvlay-greeter/           # Login-greeter idle client (ext-idle-notify → greeter/idle)
│   ├── tisvlay-tray/              # ksni tray over the SDK clients + desktop notifications
│   │                             # (notify.rs: settings service on the user socket)
│   ├── tisvlay/                   # Client SDK: DaemonClient/SessionClient/TrayClient +
│   │                             # StateWatcher (root-D-Bus-client-compatible surface)
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
│   ├── build.md                  # build, lint policy, public API policy, first-publish checklist
│   ├── book/                     # mdBook user guide (Read the Docs); src/generated/ is docgen output
│   ├── plans/                    # in-flight plan docs (README convention; git holds retired ones)
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
- CEC: the kernel keeps a logical-address claim after the handle closes, and clearing it blanks the OSD name — so the connection-holding opens clear before setting the name, and the probe/bare wake reuse a held claim (`link::Claim`)
- SSDP discovery pins source port 1901 to match the firewall rule (docs/firewall.md)
- KWin on Wayland writes DPMS-off through to the legacy sysfs `dpms` connector attribute, so the daemon polls it without a per-DE compositor protocol; the watcher gates on `status == connected`

Stream detection quirks (session-side detectors — see docs/streams/detection.md):

- No first-class "stream active" API on the Linux desktop — detection is side-effect observation (PipeWire capture node, portal `BecomeMonitor`, logind) plus app hooks. Sunshine's default NvFBC/KMS capture on Nvidia is invisible to the portal monitor *and* PipeWire; its only machine-readable signal is the unauthenticated `GET 127.0.0.1:<base port>/serverinfo` (`SUNSHINE_SERVER_BUSY|FREE`; base port 47989 by default, `--sunshine-port` otherwise — a Sunshine moved aside for Moonshine leaves Moonshine answering on 47989), polled as a level detector
- Portal `BecomeMonitor` connections are receive-only — resolve a caller's bus name → PID → exe on the *main* session-bus connection, not the monitor connection
- InputCapture portal sessions must never arm (lan-mouse holds a standing one while the user is present); it shares the portal object path + `CreateSession` member with the capture interfaces, so exclude it by interface, not heuristic
- logind mints `Remote=yes` for plain SSH shells too (`Type=tty`); the remote-*desktop* detector must require a graphical `Type` (`x11`/`wayland`)
- `pipewire-native` (the screencast detector's client) loads PipeWire's C support plugin from an SPA plugin dir baked at build time, so Nix builds need pipewire as a build input; the session checks the plugin exists before starting the detector and skips `client.conf` (`config.name = "null"`, avoiding the `spa-json-dump` tool). KWin capture nodes are matched by `media.name` prefix `kwin-screencast-`, never `node.name` (the process binary, renamed by wrappers), and `media.name` isn't in the registry's global props, so candidates are bound and judged on their full info — see docs/streams/detection.md
- NixOS `makeWrapper` truncates process comm to the kernel's 15-byte limit, so Sunshine's comm is `.sunshine-wrapp` (from `.sunshine-wrapped`) — procfs presence gates match `comm.trim_start_matches('.').starts_with("sunshine")`, not `== "sunshine"`

Zenoh stack findings (load-bearing, learned empirically):

- **Consume-only peer leaves**: a connect-only peer (resilient link) can subscribe, query, and hydrate caches, but anything it SERVES (queryables, liveliness tokens, cached pubs) is invisible to other leaves through a hub — under both peer-mode and router-mode hubs. Servers must use client-mode links (fail-fast open, supervised retry loop) into a router hub. The session service's user socket is a router for exactly this reason; the tray's settings service is a supervised client link.
- Zenoh *client* mode keeps a single link (`single_link: true`): a multi-endpoint connect list is failover, not fan-out — hence two sessions per user client (system + user socket), which also makes user/** isolation structural.
- Advanced-cache hydration is subscriber-history-only: the cache queryable sits under a verbatim `@adv` suffix, plain gets can't reach it.
- Multi-reply queries (discovery streaming) need `ConsolidationMode::None` querier-side or consolidation eats same-key replies.
- `timestamping/enabled` must be on daemon-side for cache ordering.
- Zenoh publishers guarantee no cross-key sample ordering — tests wait on combined predicates in a single drain (never two sequential waits on one subscriber).
- History replay vs live samples has no guaranteed order, and per-publisher sequencing drops replayed history once a newer live sample is delivered — so pairing progress carries a per-session `attempt` (returned by pair / re-pair) and watchers drain only theirs.
- A zenoh `get` matching zero queryables completes instantly with no reply — first-rpc-after-serve paths retry the no-reply case.
- Zenoh rejects tokio's current-thread scheduler — zenoh-touching tests need `#[tokio::test(flavor = "multi_thread")]`.
- Stale Unix socket files are fine: zenoh's `.lock` protocol takes over an unclean listener path on the next bind.
- A suspend can drop every link: systemd freezes `user.slice` around the sleep, and the monotonic 10 s lease keeps counting through the kernel's device suspend/resume (12+ s where the NVIDIA driver saves video memory), so user-side links expire on thaw and the daemon's token returns under the same session. The daemon's mirrors subscribe per token lifetime and re-hydrate on its return ([docs/architecture.md](../docs/architecture.md) § Clients).
- Reply-side backpressure is load-bearing: backend commands ack on TV completion (`BackendEvent::CommandDone`), not on enqueue — this is what makes the tray's Coalescer "latest wins" actually bound queue depth (the C3 volume-storm fix).

Deployment model:

- The daemon chmods its socket 0660 after bind (packaged unit runs UMask=0077). Access is layered: `tisvlay` group membership is the durable grant (`tisvlay enable-access` enrolls one-click via polkit), plus daemon-applied ACLs — the greeter group (`--greeter-access`, static while the daemon runs) and, presence-scoped, each seat's active graphical user (`--seat-access`, default on; keyed off logind `Seat.ActiveSession`, gated Class=user ∧ Type∈{x11,wayland} ∧ ¬Remote). Full integration detail: [packaging/README.md](../packaging/README.md) § "Socket access & the ACL integration".
- Connect-side socket defaults fall back to `/run/tisvlay/zenoh.sock` when the dev socket is absent; listen-side defaults never fall back (a dev daemon must not take over the packaged socket).
- Shell completions for the `tisvlay` CLI (bash/zsh/fish) ship in every package and the nix build. They use clap_complete's dynamic engine, so `<TAB>` completes live TV UUIDs by calling back into the binary; generation and per-distro caveats are in [packaging/README.md](../packaging/README.md).

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

All project commands live in the `justfile` at the repo root — it is the single source of truth. prek hooks, VSCode tasks, and CI workflows all dispatch through `just` so command strings are defined in exactly one place.

```bash
just              # list all recipes, grouped by purpose
just check        # deny + shear + typos + actionlint + zizmor + licenses + fmt + build + clippy + doc + test
just build        # cargo build --profile test --workspace --all-targets --all-features
just clippy       # cargo clippy (warnings denied via build.warnings)
just doc          # cargo doc --workspace --no-deps (warnings denied)
just shear        # unused / misplaced dependencies (cargo-shear)
just typos        # spell check, American English (.config/typos.toml)
just actionlint / zizmor  # workflow lint (.config/actionlint.yaml); workflow + Dependabot security audit
just test         # cargo nextest run + cargo test --doc
just test-ci      # nextest with the 'ci' profile (JUnit XML + coverage)
just cov          # coverage summary
just msrv / hack / audit  # CI-only checks, reproducible locally (docs/build.md)
just package-check  # package the three published crates in one invocation (first-publish gate)
just package-deb  # build the .deb package
just sbom-check   # per-binary SBOMs, checked against what the built binaries embed
just verify-release vX.Y.Z  # check a release's (or draft's) signatures the way users will
just docs-serve   # mdBook user guide, live (docs-build: one-shot build)
just docs-gen     # regenerate the book's generated partials (docs-gen-check: CI drift gate)
```

`just` is provided by the Nix dev shell. Outside Nix, install with `cargo install just` or your distro package manager.

## Internationalization (i18n)

`tisvlay-gui` is wired for gettext translation (Slint `@tr()` + Rust `gettext()`); v1 ships English only. The ported tray is not gettext-wired yet (tracked in docs/ideas/switchover-followups.md). Architecture and workflow: [docs/i18n.md](../docs/i18n.md). After adding or changing user-facing GUI strings, run `just i18n-extract` and commit the refreshed `.pot` diff.

- **Translate**: button labels, pane headers, status text, menu items, tooltip copy, error messages.
- **Don't translate**: brand names (`"TisVlay"`, `"LG WebOS"`), machine identifiers (`"awaiting_tv"`, `"lg"`), Unicode glyphs, technical placeholders, file paths, URLs.
- **Format strings**: use `gettext("Foo: {0}").replace("{0}", &v.to_string())` — never `format!`, which `xgettext` can't extract from.

## Linting & CI

All checks are defined in `prek.toml`, which dispatches to `just` recipes. Hook ids match the corresponding `just` recipe names, ordered by priority: file checks → `fmt` → `build` → `clippy`/`doc` → `test`. Build and clippy use `--profile test` so `cargo test` reuses all build artifacts. The gate recipes deny warnings by exporting `CARGO_BUILD_WARNINGS=deny` — never set in `.cargo/config.toml`, which the Nix package build reads; the policy lives in `docs/build.md` § Lint policy.

```bash
prek run -a                                          # all checks
prek run -a --skip build --skip clippy --skip test   # fast file checks + fmt only
```

CI (`.github/workflows/ci.yml`) gates its jobs on what a change touches: a `changes` job classifies the diff, so docs-only changes run only the fast `checks` job (prek file checks, typos, actionlint + zizmor), plus `docs` when the book is touched, and draft PRs run only `checks` (`docs/build.md` § What runs when). Code changes also run a single `just test-ci` (clippy + nextest + doc tests + coverage under one instrumented build, `--all-features`), plus `msrv`, `features` (`just hack`) and `docs` (`just docs-gen-check` + `just docs-links`) jobs. Design notes: `docs/build.md`.

### Code conventions

Rules the lints can't fully express; the lint policy and its reasons are in `docs/build.md` § Lint policy.

- **Panics state their invariant.** `expect("… because …")`, never a bare `unwrap()` in shipped code. Tests may unwrap.
- **Errors:** published crates and the client SDK expose `thiserror` types callers can match on. Binaries and binary-glue libraries (`tisvlay-runtime`, the service crates' library halves) may use `anyhow`. The model is in `docs/architecture.md` § Error model.
- **Logging:** through `tracing`, with structured fields (`info!(%tv, "connected")`) rather than values formatted into the message. No `println!`/`eprintln!` outside the CLI (lint-enforced).
- **Secrets never reach `Debug`.** A typed secret is a `tisvlay_core::Secret`: its `Debug` prints `[REDACTED]`, serde sees a plain string, and the value is read through `expose()`. The LG client key is one everywhere it's typed (`Credentials`, `LgVendorConfig`, the register payloads, `WebOsClient`); a new credential field uses it too. The untyped vendor config is a `tisvlay_core::VendorConfig`, and the wire requests that carry one (`AddTv`, `UpdateConfig`) are hand-written the same way: their `Debug` prints key names only. `Secret` wraps secrecy 0.10 (redacted `Debug`, zeroize on drop, `Serialize` opt-in); zenoh-config still pins 0.8, so `deny.toml` skips that duplicate.
- **Handles whose drop cancels work** (watch handles, the service stacks, liveliness guards) carry `#[must_use = "…"]` saying what dropping them does.
- **Published crates** (core, protocol, and the `tisvlay` SDK) keep items private by default. `#[non_exhaustive]`, public-dependency and README rules are in `docs/build.md` § Public API policy; `just package-check` packages them.
- **A wire-visible change is a `feat` or a breaking commit, never a `fix`:** any change to a key, payload, rpc or decode rule. Clients and services check each other's release on `major.minor`, and release-plz bumps the minor only for those commit types, so a protocol change shipped as a patch would pass the check and fail at runtime.
- **A new rpc is declared once** in its family's table in `tisvlay-protocol`'s `endpoint` module (key, request, response, read or write); the compiler then demands the server arm. A world-answered rpc also gets a `Command` variant, and the reactor's exhaustive match then demands its one-shot handler in `tisvlay-world`'s `systems/commands.rs`. Add the SDK or CLI surface by hand. Requests decode strictly: no compatibility shims on the wire.
- **Allowances:** reasoned `#[expect(lint, reason = "…")]`. Use `#[allow]` with a reason only where the lint depends on features or tests.
- **`unsafe`:** only in files that carry a reasoned `#![expect(unsafe_code)]`, one operation per block, each with a `// SAFETY:` comment.

### Dead Code Detection

Public items are invisible to Rust's `dead_code` lint. Use `cargo workspace-unused-pub` periodically after major refactors; false positives include trait impl methods required by frameworks (rustls, ksni, zenoh handlers) and builder methods reserved for future use.

### Testing

See [docs/testing.md](../docs/testing.md) for the testing guide. Layers:

- `tisvlay-test` — OS-environment fakes (private `dbus-daemon`, fake logind, fake ScreenSaver) for the consumed-D-Bus adapters, and a `Relay` that cuts, stalls or drops a batch on a zenoh link while the service keeps running.
- `tisvlay-mock` — fake vendor backend (implements `tisvlay-core` traits), always registered in the daemon.
- Wire-level integration tests live in each binary crate's `tests/` — one test binary per crate, a multi-suite crate's suites as modules of `tests/it/` (daemon: wire/profiles/cec/greeter/logind/liveliness/access; SDK: client + conformance), single-suite crates as one `tests/<name>.rs` (session, tray settings) — they boot real daemon/session stacks on temp sockets, no fixture crate needed. The SDK's conformance suite runs every mirror through the same link-breaking cases, against a fake service or the real stacks.

```bash
just test                   # cargo nextest run + doc tests
just cov / cov-html / cov-ratchet
```

## Reference Documentation

- [docs/architecture.md](../docs/architecture.md) - **Current architecture**: topology, key space, world/reactor model, compatibility (release handshake), deployment, empirical zenoh laws
- [docs/book/](../docs/book/) - User guide (mdBook, published on Read the Docs): install, pairing, profiles, automation, streams, operations
- [docs/build.md](../docs/build.md) - Build and lint policy, public API policy, first-publish checklist, review triggers
- [docs/streams/detection.md](../docs/streams/detection.md) - Stream detectors (PipeWire, portal, logind, Sunshine) and their quirks
- [docs/design.md](../docs/design.md) - The retired served-D-Bus architecture (≤ v0.2.0); kept for history
- [docs/discovery.md](../docs/discovery.md) - SSDP transport, streaming discovery, manual-entry MAC enrichment
- [docs/i18n.md](../docs/i18n.md) - Translation architecture and workflow
- [docs/testing.md](../docs/testing.md) - Testing guide
- [docs/lg/firmware-quirks.md](../docs/lg/firmware-quirks.md) - LG WebOS firmware quirks
- [docs/cec/protocol.md](../docs/cec/protocol.md) - HDMI-CEC backend model
- [docs/cec/testing.md](../docs/cec/testing.md) - CecLink seam + FakeCecLink
- [docs/watchers/network.md](../docs/watchers/network.md) - Carrier detection, route resolution
- [docs/firewall.md](../docs/firewall.md) - SSDP firewall configuration
- [docs/ideas/](../docs/ideas/README.md) - Deferred work + research notes
- [docs/concerns/](../docs/concerns/README.md) - Design concerns + technical debt
- [docs/bugs/](../docs/bugs/README.md) - Known bugs
