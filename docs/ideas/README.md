# Ideas

Deferred-work index. Each entry is a self-contained spec with rationale, path forward, and trade-offs — pick one when its trigger condition fires (a user reports the gap, a related bug surfaces, a sprint has slack).

## Architecture

- [Event-driven power-management redesign](event-driven-power-management.md)
- [Greeter integration generalization (greetd+wlroots, GDM, non-NixOS)](greeter-integration-generalization.md)
- [TV-side power prevention (manage LG auto-off Luna settings)](tv-side-power-prevention.md)
- [Richer `DispatchOutcome` return type for screen_on / screen_off](dispatch-outcome-return-type.md)
- [Power-on for an already-connected TV (startup `PowerOn` warning, LG capability mismatch, possible dark panel)](connected-power-on.md)
- [Idempotent write rpcs via client-minted ids (extend caller re-issue to writes)](idempotent-write-rpcs.md)
- [Virtual DRM connector for detached TVs](virtual-drm-connector.md)
- [CEC backend follow-ups (relative volume, multi-transport identity, DPMS-mirror over USB-CEC, emulated-screen toggle, manual-entry label, vendor-OUI branding, inputattach start limit)](cec-followups.md)
- [Stable device alias for USB CEC adapters (a udev symlink so configs survive node renumbering)](cec-stable-device-alias.md)
- [TV-driven PC power and input (what the TV-controls-the-PC plan left unplanned: input-change events, more than one TV, a leave-alone wake value, the EEPROM lever and a driver patch, LG remote keys and pointer)](tv-controls-pc.md)
- [Power and activity vocabulary (the renames still owed by older names; the glossary itself now lives in architecture.md)](power-vocabulary.md)
- [Strict decoding of unknown fields (requests, stores and configs skip unknown fields today, so typos and renamed fields vanish silently; open questions on which surfaces, store failure, renames)](strict-decoding.md)
- [Probe library extraction (when GUI lands)](probe-library-extraction.md)
- [SSDP `NOTIFY` passive listener](ssdp-notify-passive-listener.md)
- [Pair-time MAC capture (ARP snoop for vendors without a device-reported MAC)](pair-time-mac-capture.md)
- [WoL via raw socket (CAP_NET_RAW)](wol-raw-socket.md)
- [Polkit gating for multi-user shared workstations](polkit-multi-user.md)
- [`a{sv}` wire-format migration for vendor-config surfaces](asv-wire-format-migration.md)
- [Typed pairing-failure follow-ups (more dial codes, handshake device errors, GUI recovery actions)](typed-failed-reason-payload.md)
- [Notification-subsystem follow-up cleanups](notification-subsystem-followups.md)
- [Shared supervised-task helper (tray link / idle adapter / netlink monitor / seat and logind watches)](supervised-task-helper.md)
- [Eager remote-state refresh on gated events (world-side DPMS path)](eager-remote-state-refresh.md)
- [Stream detection follow-ups (Steam/Parsec + udev detectors, GNOME port, tray quick-toggle, allow-list GUI, Sunshine poll off-switch, PipeWire test fake)](stream-followups.md)
- [Screencast detection beyond KWin (no blessed cross-desktop signal; per-producer, class-and-producer, compositor-PID and Hyprland options; upstream `media.role=Screen` and portal-monitor proposals; NixOS VM test)](screencast-detection-coverage.md)
- [Sit the rpc layer on zenoh-ext's typed queryables (if zenoh#2510 lands)](zenoh-typed-queryables.md)
- [A longer lease on the Unix-socket links, so a sleep doesn't drop them](zenoh-link-lease.md)
- [SDK methods for the rpcs the GUI can't reach (add-tv, discover, user effective)](sdk-cli-only-rpcs.md)
- [A typed `update-config` request (settings fields + flattened vendor map)](typed-update-config.md)

## Operational

- [User service auto-start after `nixos-rebuild switch`](nixos-user-service-auto-start.md)
- [Opt-in CEC setup for distro packages (device access and Pulse-Eight attach)](cec-distro-setup.md)

## CLI

- [`key=value` shorthand for `tisvlay config`](cli-update-config-shorthand.md)
- [Support-bundle CLI subcommand (`tisvlay diagnostic`)](support-bundle-cli.md)
- [Shell-completion follow-ups: no-daemon knob, UUID cache, non-default socket/site](completion-followups.md)
- [TV names as CLI selectors (accept a name wherever a TV UUID is taken)](cli-tv-name-selectors.md)

## GUI

- [DE-driven theming via xdg-desktop-portal Settings](de-driven-theming.md)
- [Slint dialog open/close fade animation](slint-dialog-fade-animation.md)
- ["Open logs" affordance](open-logs-affordance.md)
- [Re-enable Slint `accessibility` feature once upstream blockers clear](re-enable-slint-accessibility.md)
- [GUI test coverage fan-out (Layer-C end-to-end tests)](gui-test-coverage-fanout.md)
- [Theme-icon upgrade for the default-profile indicator](profile-default-emblem-theme-icon.md)
- [Contextual-help pencil/info double-affordance density](contextual-help-followups.md)

## Build & CI

- [Mutation testing with cargo-mutants (scoped to pure-logic crates, on demand)](mutation-testing.md)
- [Fuzzing the parsers (grow the never-panic properties into bolero fuzz targets)](parser-fuzzing.md)
- [Deterministic network simulation (turmoil) for the LG reconnect logic](deterministic-network-tests.md)
- [Switchover follow-ups: tray gettext, mock-backend gating, coverage baseline, fresh design doc](switchover-followups.md)
- [Move the pre-publish artifact smoke-test into CI](ci-artifact-smoke-test.md)
- [Switch CI (or workspace) to nightly Rust for build-time knobs](ci-nightly-build-knobs.md)
- [Replace hardcoded sleeps in dispatch integration tests with poll-with-deadline](dispatch-test-event-driven-waits.md)
- [Seat-watch regression coverage over a fake logind Seat surface](seat-watch-fake-logind-coverage.md)
- [Cache `main` builds in tisvlay.cachix.org (closure-only push, release pins)](nix-cache-main-builds.md)
- [SBOM file inside the distro packages (no install-location convention yet; scanners read the auditable binaries)](in-package-sbom.md)
- [OpenSSF Scorecard (zizmor already gates the workflow checks; what's left is a badge and repo settings)](openssf-scorecard.md)
- [SBOM precision (features unified across binaries; Slint's elected license not recorded)](sbom-precision.md)

## Diagnostics & telemetry

- [Diagnostic log-tail ring buffer](diagnostic-log-tail.md)
- [Diagnostic preview-and-consent pane](diagnostic-preview-consent-pane.md)
- [Crash-time diagnostic capture](crash-time-diagnostic-capture.md)
- [Opt-in remote crash reporting (`dev-telemetry` feature)](dev-telemetry-remote-crash-reporting.md)
- [Native crash capture via `minidumper`](native-crash-capture-minidumper.md)
