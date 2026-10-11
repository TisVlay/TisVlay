# Ideas

Deferred-work index. Each entry is a self-contained spec with rationale, path forward, and trade-offs — pick one when its trigger condition fires (a user reports the gap, a related bug surfaces, a sprint has slack).

## Architecture

- [Event-driven power-management redesign](ideas/event-driven-power-management.md)
- [Greeter integration generalization (greetd+wlroots, GDM, non-NixOS)](ideas/greeter-integration-generalization.md)
- [TV-side power prevention (manage LG auto-off Luna settings)](ideas/tv-side-power-prevention.md)
- [Richer `DispatchOutcome` return type for screen_on / screen_off](ideas/dispatch-outcome-return-type.md)
- [Power-on for an already-connected TV (startup `PowerOn` warning, LG capability mismatch, possible dark panel)](ideas/connected-power-on.md)
- [Idempotent write rpcs via client-minted ids (extend caller re-issue to writes)](ideas/idempotent-write-rpcs.md)
- [Virtual DRM connector for detached TVs](ideas/virtual-drm-connector.md)
- [CEC backend follow-ups (relative volume, multi-transport identity, DPMS-mirror over USB-CEC, emulated-screen toggle, manual-entry label, vendor-OUI branding)](ideas/cec-followups.md)
- [Probe library extraction (when GUI lands)](ideas/probe-library-extraction.md)
- [SSDP `NOTIFY` passive listener](ideas/ssdp-notify-passive-listener.md)
- [Pair-time MAC capture (ARP snoop for vendors without a device-reported MAC)](ideas/pair-time-mac-capture.md)
- [WoL via raw socket (CAP_NET_RAW)](ideas/wol-raw-socket.md)
- [Polkit gating for multi-user shared workstations](ideas/polkit-multi-user.md)
- [`a{sv}` wire-format migration for vendor-config surfaces](ideas/asv-wire-format-migration.md)
- [Typed `Failed { reason }` payload across daemon error vocabulary](ideas/typed-failed-reason-payload.md)
- [Notification-subsystem follow-up cleanups](ideas/notification-subsystem-followups.md)
- [Shared supervised-task helper (tray link / idle adapter / netlink monitor)](ideas/supervised-task-helper.md)
- [Eager remote-state refresh on gated events (world-side DPMS path)](ideas/eager-remote-state-refresh.md)
- [Remote-session detection follow-ups (Steam/Parsec + udev detectors, GNOME port, tray quick-toggle, allow-list GUI, Sunshine poll off-switch, PipeWire test fake)](ideas/remote-session-followups.md)
- [Sit the rpc layer on zenoh-ext's typed queryables (if zenoh#2510 lands)](ideas/zenoh-typed-queryables.md)
- [SDK methods for the rpcs only the CLI reaches (add-tv, discover, user effective)](ideas/sdk-cli-only-rpcs.md)
- [Prune core's unused error vocabularies (`PowerError`, `PowerErrorCode`, `ConfigErrorCode`)](ideas/core-error-vocabulary-pruning.md)
- [Reload config from disk (daemon, session, tray): the dead "Reload config" button](ideas/reload-config.md)
- [A typed `update-config` request (settings fields + flattened vendor map)](ideas/typed-update-config.md)

## Operational

- [User service auto-start after `nixos-rebuild switch`](ideas/nixos-user-service-auto-start.md)

## CLI

- [CLI shorthand for `Tv1.UpdateConfig`](ideas/cli-update-config-shorthand.md)
- [Support-bundle CLI subcommand (`tisvlay diagnostic`)](ideas/support-bundle-cli.md)
- [Shell-completion follow-ups: no-daemon knob, UUID cache, non-default socket/site](ideas/completion-followups.md)

## GUI

- [DE-driven theming via xdg-desktop-portal Settings](ideas/de-driven-theming.md)
- [Slint dialog open/close fade animation](ideas/slint-dialog-fade-animation.md)
- ["Open logs" affordance](ideas/open-logs-affordance.md)
- [Re-enable Slint `accessibility` feature once upstream blockers clear](ideas/re-enable-slint-accessibility.md)
- [GUI test coverage fan-out (Layer-C end-to-end tests)](ideas/gui-test-coverage-fanout.md)
- [Theme-icon upgrade for the default-profile indicator](ideas/profile-default-emblem-theme-icon.md)
- [Contextual-help pencil/info double-affordance density](ideas/contextual-help-followups.md)

## Build & CI

- [Mutation testing with cargo-mutants (scoped to pure-logic crates, on demand)](ideas/mutation-testing.md)
- [Fuzzing the parsers (grow the never-panic properties into bolero fuzz targets)](ideas/parser-fuzzing.md)
- [Deterministic network simulation (turmoil) for the LG reconnect logic](ideas/deterministic-network-tests.md)
- [Switchover follow-ups: tray gettext, mock-backend gating, coverage baseline, fresh design doc](ideas/switchover-followups.md)
- [Move the pre-publish artifact smoke-test into CI](ideas/ci-artifact-smoke-test.md)
- [Switch CI (or workspace) to nightly Rust for build-time knobs](ideas/ci-nightly-build-knobs.md)
- [Replace hardcoded sleeps in dispatch integration tests with poll-with-deadline](ideas/dispatch-test-event-driven-waits.md)
- [Seat-watch regression coverage over a fake logind Seat surface](ideas/seat-watch-fake-logind-coverage.md)

## Diagnostics & telemetry

- [Diagnostic log-tail ring buffer](ideas/diagnostic-log-tail.md)
- [Diagnostic preview-and-consent pane](ideas/diagnostic-preview-consent-pane.md)
- [Crash-time diagnostic capture](ideas/crash-time-diagnostic-capture.md)
- [Opt-in remote crash reporting (`dev-telemetry` feature)](ideas/dev-telemetry-remote-crash-reporting.md)
- [Native crash capture via `minidumper`](ideas/native-crash-capture-minidumper.md)
- [Triage script: capture `PowerCycleProgress` phases for panel-lit measurement](ideas/triage-power-cycle-progress-phases.md)
