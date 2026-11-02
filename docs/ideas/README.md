# Ideas

Deferred-work index. Each entry is a self-contained spec with rationale, path forward, and trade-offs — pick one when its trigger condition fires (a user reports the gap, a related bug surfaces, a sprint has slack).

## Architecture

- [Event-driven power-management redesign](event-driven-power-management.md)
- [Greeter integration generalization (greetd+wlroots, non-NixOS; GDM shipped)](greeter-integration-generalization.md)
- [TV-side power prevention (manage LG auto-off Luna settings)](tv-side-power-prevention.md)
- [Richer `DispatchOutcome` return type for screen_on / screen_off](dispatch-outcome-return-type.md)
- [Idempotent write rpcs via client-minted ids (extend caller re-issue to writes)](idempotent-write-rpcs.md)
- [Virtual DRM connector for detached TVs](virtual-drm-connector.md)
- [CEC backend follow-ups (relative volume, multi-transport identity, DPMS-mirror over USB-CEC, emulated-screen toggle, manual-entry label, vendor-OUI branding, inputattach start limit)](cec-followups.md)
- [Stable device alias for USB CEC adapters (a udev symlink so configs survive node renumbering)](cec-stable-device-alias.md)
- [TV-driven PC power and input (what the TV-controls-the-PC plan left unplanned: input-change events, more than one TV, a leave-alone wake value, the EEPROM lever and a driver patch, LG remote keys and pointer)](tv-controls-pc.md)
- [Wake follow-ups (a dialing wake an off doesn't cancel, a screen intent's in-place wake without `dispatch-failed`, LG's dial-phase resend rate, the in-place failure toast, LG's wait for its first push, a power-on as an in-place wake ends, pushes during an in-place wake, untested hardware paths)](wake-followups.md)
- [Power and activity vocabulary (the renames still owed by older names; the glossary itself now lives in architecture.md)](power-vocabulary.md)
- [Strict decoding of unknown fields (requests, stores and configs skip unknown fields today, so typos and renamed fields vanish silently; open questions on which surfaces, store failure, renames)](strict-decoding.md)
- [SSDP `NOTIFY` passive listener](ssdp-notify-passive-listener.md)
- [mDNS/DNS-SD discovery (no firewall rule needed on most systems; a path to IPv6 discovery)](mdns-discovery.md)
- [Pair-time MAC capture (ARP snoop for vendors without a device-reported MAC)](pair-time-mac-capture.md)
- [WoL via raw socket (CAP_NET_RAW)](wol-raw-socket.md)
- [Polkit gating for multi-user shared workstations](polkit-multi-user.md)
- [Typed pairing-failure follow-ups (more dial codes, handshake device errors, GUI recovery actions)](typed-failed-reason-payload.md)
- [Tray notification settings: a partial `tray.toml` doesn't parse (per-field defaults, or document it strict)](notification-subsystem-followups.md)
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
- [Theme-icon upgrade for the default-profile indicator](profile-default-emblem-theme-icon.md)
- [Contextual-help pencil/info double-affordance density](contextual-help-followups.md)

## Build & CI

- [Mutation testing with cargo-mutants (scoped to pure-logic crates, on demand)](mutation-testing.md)
- [Fuzzing the parsers (grow the never-panic properties into bolero fuzz targets)](parser-fuzzing.md)
- [Deterministic network simulation (turmoil) for the LG reconnect logic](deterministic-network-tests.md)
- [Switchover follow-ups: tray gettext, mock-backend gating, IPC performance harness, fresh design doc](switchover-followups.md)
- [Move the pre-publish artifact smoke-test into CI](ci-artifact-smoke-test.md)
- [Switch CI (or workspace) to nightly Rust for build-time knobs (measured and rejected for now)](ci-nightly-build-knobs.md)
- [Cache `main` builds in tisvlay.cachix.org (closure-only push, release pins)](nix-cache-main-builds.md)
- [SBOM file inside the distro packages (no install-location convention yet; scanners read the auditable binaries)](in-package-sbom.md)
- [OpenSSF Scorecard (zizmor already gates the workflow checks; what's left is a badge and repo settings)](openssf-scorecard.md)
- [SBOM precision (features unified across binaries; Slint's elected license not recorded)](sbom-precision.md)
- [More generated references in the book (NixOS module options, per-vendor pairing fields and capabilities)](book-generated-references.md)

## Diagnostics & telemetry

- [Diagnostic log-tail ring buffer](diagnostic-log-tail.md)
- [Diagnostic preview-and-consent pane](diagnostic-preview-consent-pane.md)
- [Crash-time diagnostic capture](crash-time-diagnostic-capture.md)
- [Opt-in remote crash reporting (`dev-telemetry` feature)](dev-telemetry-remote-crash-reporting.md)
- [Native crash capture via `minidumper`](native-crash-capture-minidumper.md)
