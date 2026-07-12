# Ideas

Deferred-work index. Each entry is a self-contained spec with rationale, path forward, and trade-offs — pick one when its trigger condition fires (a user reports the gap, a related bug surfaces, a sprint has slack).

## Architecture

- [Event-driven power-management redesign](ideas/event-driven-power-management.md)
- [Greeter integration generalization (greetd+wlroots, GDM, non-NixOS)](ideas/greeter-integration-generalization.md)
- [TV-side power prevention (manage LG auto-off Luna settings)](ideas/tv-side-power-prevention.md)
- [Richer `DispatchOutcome` return type for screen_on / screen_off](ideas/dispatch-outcome-return-type.md)
- [Virtual DRM connector for detached TVs](ideas/virtual-drm-connector.md)
- [CEC backend follow-ups (relative volume, multi-transport identity, DPMS-mirror over USB-CEC, emulated-screen toggle, manual-entry label)](ideas/cec-followups.md)
- [Probe library extraction (when GUI lands)](ideas/probe-library-extraction.md)
- [SSDP `NOTIFY` passive listener](ideas/ssdp-notify-passive-listener.md)
- [Pair-time MAC capture](ideas/pair-time-mac-capture.md)
- [WoL via raw socket (CAP_NET_RAW)](ideas/wol-raw-socket.md)
- [Polkit gating for multi-user shared workstations](ideas/polkit-multi-user.md)
- [`a{sv}` wire-format migration for vendor-config surfaces](ideas/asv-wire-format-migration.md)
- [Typed `Failed { reason }` payload across daemon error vocabulary](ideas/typed-failed-reason-payload.md)
- [Notification-subsystem follow-up cleanups](ideas/notification-subsystem-followups.md)
- [Shared supervised-task helper (tray link / idle adapter / netlink monitor)](ideas/supervised-task-helper.md)
- [Eager remote-state refresh on gated events (world-side DPMS path)](ideas/eager-remote-state-refresh.md)
- [Remote-session detection follow-ups (Steam/Parsec + udev detectors, GNOME port, tray quick-toggle, allow-list GUI, configurable Sunshine endpoint, PipeWire test fake)](ideas/remote-session-followups.md)

## Operational

- [User service auto-start after `nixos-rebuild switch`](ideas/nixos-user-service-auto-start.md)
- [Atomic config writes (tempfile + rename)](ideas/atomic-config-writes.md)

## CLI

- [CLI shorthand for `Tv1.UpdateConfig`](ideas/cli-update-config-shorthand.md)
- [Support-bundle CLI subcommand (`tisvlay diagnostic`)](ideas/support-bundle-cli.md)

## GUI

- [DE-driven theming via xdg-desktop-portal Settings](ideas/de-driven-theming.md)
- [Slint dialog open/close fade animation](ideas/slint-dialog-fade-animation.md)
- ["Open logs" affordance](ideas/open-logs-affordance.md)
- [Re-enable Slint `accessibility` feature once upstream blockers clear](ideas/re-enable-slint-accessibility.md)
- [GUI test coverage fan-out (Layer-C end-to-end tests)](ideas/gui-test-coverage-fanout.md)
- [Theme-icon upgrade for the default-profile indicator](ideas/profile-default-emblem-theme-icon.md)
- [Contextual-help pencil/info double-affordance density](ideas/contextual-help-followups.md)

## Build & CI

- [Switchover follow-ups: tray gettext, mock-backend gating, coverage baseline, fresh design doc](ideas/switchover-followups.md)
- [Move the pre-publish artifact smoke-test into CI](ideas/ci-artifact-smoke-test.md)
- [Switch CI (or workspace) to nightly Rust for build-time knobs](ideas/ci-nightly-build-knobs.md)
- [MSRV verification job (keep the `rust-version` floor honest under the pinned channel)](ideas/msrv-verification-job.md)
- [Replace hardcoded sleeps in dispatch integration tests with poll-with-deadline](ideas/dispatch-test-event-driven-waits.md)

## Diagnostics & telemetry

- [Diagnostic log-tail ring buffer](ideas/diagnostic-log-tail.md)
- [Diagnostic preview-and-consent pane](ideas/diagnostic-preview-consent-pane.md)
- [Crash-time diagnostic capture](ideas/crash-time-diagnostic-capture.md)
- [Opt-in remote crash reporting (`dev-telemetry` feature)](ideas/dev-telemetry-remote-crash-reporting.md)
- [Native crash capture via `minidumper`](ideas/native-crash-capture-minidumper.md)
- [Triage script: capture `PowerCycleProgress` phases for panel-lit measurement](ideas/triage-power-cycle-progress-phases.md)
