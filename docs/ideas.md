# Ideas

## User service auto-start after `nixos-rebuild switch`

NixOS's activation script restarts **system** services on `switch`, but
not **user** services — those only pick up changes on next login. Other
projects (e.g. NordVPN) work around this by having their desktop GUI
detect that the companion user service isn't running and start it on
demand.

Options for TisVlay:

1. **D-Bus activation for the user session** — ship an
   `org.tisvlay.Session1.service` D-Bus user service file so that when
   `tisvlay-tray` (or anything else) calls the session interface on the
   user bus, systemd auto-starts `tisvlay-session`.

2. **Tray/GUI starts the session service** — have `tisvlay-tray` (or the
   future `tisvlay-gui`) call `systemctl --user start tisvlay-session`
   when it detects the service isn't running. This is closest to what
   NordVPN's desktop client does.

3. **`restartTriggers` in the NixOS module** — add a trigger so the
   activation script kicks user services after `switch`. Fragile because
   it requires enumerating logged-in sessions via `loginctl`.

Option 2 is the most natural fit given the `tisvlay-gui` roadmap — the
GUI/tray becomes the entry point that ensures the rest of the user-side
stack is running.

## Event-driven power-management redesign

Today's `tisvlay-session` listener path (D-Bus screensaver, Wayland idle, logind, idle timer) drives `Tv1.Power.SetIsScreenOn` / `PowerOff` per session events. It works but couples each new event source to a session-bus dependency, and the lock/unlock/idle policy is split between TOML config and `event_handler` code. A redesign should preserve the current UX exactly while opening the door to per-DE adapters with cleaner event semantics.

### Hard requirements

- **Sub-100 ms lockscreen-snap-back.** Mouse jiggle on lockscreen → TV unblanks before the user perceives lag. Today's Wayland idle path delivers this; any replacement must too.
- **Distinguish fast blank from slow power-off.** `Tv1.Power.SetIsScreenOn(false)` keeps the TV powered (~100 ms LG round-trip on resume); `PowerOff` deep-sleeps (5-10 s LG OLED cold boot, requires WoL on wake). Map: lockscreen blank / idle blank → `IsScreenOn(false)`; system suspend / explicit user action / long-idle escalation → `PowerOff`.
- **Stay multi-TV.** Per-TV instances keyed by EDID-matched DRM connector (the existing `Tv1.Desktop.Connector` surface).
- **Handle detached TVs (HDMI splitter, network-only) via per-TV idle timer in the daemon, resets on user-initiated activity.**

### Candidate event sources, ranked by latency

| Source | Latency | Coverage |
|---|---|---|
| KWin Wayland idle (`org_kde_kwin_idle`) | ~10 ms | KWin only |
| wlroots output management (`wlr_output_management_v1`) | ~10 ms | sway, hyprland, river |
| KWin DPMS (`org_kde_kwin_dpms`) | ~10 ms | KWin only |
| logind `LockedHint` / `IdleHint` PropertiesChanged | ~50 ms | KDE; GNOME inconsistent |
| DRM atomic API (non-master client) | polling-only | any compositor on Linux |
| Sysfs `/sys/class/drm/*/dpms` poll | 1 / poll-rate | any compositor on Linux |
| udev DRM uevents | event-driven for hotplug, NOT for DPMS prop changes | any compositor on Linux |

### Likely architecture

A small per-DE adapter pattern:

```
tisvlayd
 ├── PowerObserver trait
 │    ├── KdeWaylandObserver (kde_kwin_idle + dpms)
 │    ├── WlrootsObserver (wlr_output_management)
 │    ├── X11Observer (XScreenSaver extension)
 │    └── LogindFallbackObserver (LockedHint + DPMS sysfs poll)
 │         — fallback when no specific adapter matches
 └── Each observer emits ScreenBlank / ScreenWake / Sleep / Wake
    events on a single channel; daemon dispatches to Tv1.Power.*
```

The session crate either gets folded into `tisvlayd` (its session-bus connection becomes the `KdeWaylandObserver`'s home) or one of the adapters runs as a small user-context helper. Depends on whether system-bus tisvlayd can hold a Wayland connection — needs investigation.

### What NOT to do

- **Don't ship sysfs-DPMS polling as the primary path.** DPMS doesn't distinguish blank from off in modern compositors — KDE/X11 collapse to On/Off in practice. Mapping DPMS_OFF to `Tv1.Power.PowerOff` loses the fast-blank UX (panel toggle, ~100 ms) and replaces it with a TV cold-boot (5-10 s). Mapping DPMS_OFF to `IsScreenOn(false)` works correctness-wise, but the wake-from-lockscreen latency is capped by the polling rate (~1 s at 1 Hz vs ~100 ms today).
- **Don't unify "blank" and "off" semantics.** Two distinct LG SSAP code paths with different physics. Pick per-event.
- **Don't drop the per-DE adapter pattern in favor of "one universal observer".** Each DE exposes session state differently and there's no Linux-wide standard event.

### Lineage

Volume sync via PipeWire and brightness sync via KWin Wayland + D-Bus + an out-of-tree kernel module were explored in [#34](https://github.com/ruffsl/TisVlay/pull/34) and scrapped before merging to main. Reasons worth preserving so this isn't re-discovered:

- **Brightness:** PowerDevil patches required (BacklightDetector EDID extension + udev hotplug + KWinDisplayDetector eviction). Distributing a binary patch against Plasma's release cadence is an ongoing maintenance burden. KWin's `kde_external_brightness_v1` isn't truly bidirectional — `set_observed_brightness` is one-shot per drag (commit-gated by `m_done`), so a Wayland-only path can't reflect TV-remote changes back onto the slider. HDR mode bypasses the protocol, switching to a D-Bus path with different semantics. D-Bus reverse loops require fragile race-free guards (echo-suppress, ramp-debounce) that produce visible UI bouncing under stress.
- **Volume:** PipeWire mirror chains two gain stages, and the result depends on which side moved last. Per-app audio gets clipped or attenuated unpredictably.

The full sync history (granular commits, plan docs, removed kernel module) is preserved in [PR #34](https://github.com/ruffsl/TisVlay/pull/34); the cull that retired it is preserved in the squash-merge that brought this stub onto main.

## Virtual DRM connector for detached TVs

For users with TVs not on a PC GPU (HDMI splitter, network-only, multi-room), the long-term desire is for the compositor to manage them as if they were physical displays — i.e. the compositor sees a virtual display, and the daemon observes DPMS / wake events on it the same way it observes physical connectors.

Three paths for future research:

1. **Out-of-tree minimal virtual DRM driver.** A small kernel module that registers a `drm_device` with one or more `drm_connector` entries, no CRTC / encoder / framebuffer scan-out machinery. Connectors expose `dpms`, `edid`, `status` like real connectors. Compositor manages DPMS state on them. Daemon observes via the same path as a physical connector. Each managed-but-detached TV gets its own connector. EDID is synthesized from TV identity (or the user pins one). Cost: significant. DRM is complex; even a "minimal" connector driver needs to interact correctly with DRM master semantics, modesetting (compositors will at least probe modes), and the userspace API surface enough for compositors not to choke. `vkms` (in-tree virtual KMS) is the reference — too feature-heavy to fork wholesale, but illustrates the integration shape. Rust-for-Linux DRM bindings are still emerging (the nova-driver work in 6.13+ is the leading edge); a C-only or mostly-C driver is the realistic path today.

2. **Userspace virtual-connector bridge.** If a future kernel grows a "virtual connector userspace driver" interface (analogous to `uinput` for input devices), userspace could synthesize a DRM connector without kernel module work. No such interface exists today.

3. **Extend X11 / Wayland protocols.** Lobby for a Wayland protocol that lets non-master clients receive output state notifications across all connectors. Strictly upstream effort, no in-tree work for tisvlay.

The expectation is that someone hits the detached-TV use case in practice, gauges the cost-benefit, and either builds option 1 or accepts a per-TV idle-timer fallback in the event-driven redesign above.

## Probe library extraction (when GUI lands)

`crates/tisvlayd/src/desktop_probe.rs` walks `/sys/class/drm/`, parses each connector's EDID via `edidkit`, and matches managed TVs to physical connectors by `HardwareId`. The daemon owns the probe internally — it triggers on every `ConnectionChanged(true)` event (when a freshly-connected TV's `HardwareId` first becomes available) and on `Manager1.ReloadConfig`. The matching outcome is exposed via `Tv1.Desktop.Connector` on D-Bus.

When `tisvlay-gui` lands and wants richer EDID information (capability tables, manufacture date, vendor-specific extension blocks, HDR static metadata, supported modes, etc.) for use cases like "show the right vendor icon", "enable HDR-only setting choices", or "render a per-TV capabilities pane", the right move is a library extraction rather than growing the D-Bus surface:

- **Extract the probe primitives into a reusable library crate** (e.g. `tisvlay-probe`, or fold into `tisvlay-core` behind a feature flag if its deps stay narrow). Expose pure functions: `enumerate_connectors() -> Vec<ProbedConnector>`, `read_edid(connector_path) -> Result<Vec<u8>>`, `parse_edid(bytes) -> ParsedEdid` (full field surface — capabilities, manufacture date, vendor blocks), `parse_hardware_id(bytes) -> HardwareId` (the subset already used for matching).
- **`tisvlayd` keeps owning the matching responsibility** via the library. No behavioral change.
- **`tisvlay-gui` consumes the library directly** for on-demand introspection. Reads `Tv1.Desktop.Connector` from D-Bus to know which connector to point at, then calls the library locally to render rich detail.
- **D-Bus surface stays minimal**: matching outcome + identity essentials. Capabilities, mode lists, vendor blocks, manufacture metadata never flow through zbus.

### Why not push everything through D-Bus

D-Bus is a synchronization protocol for cross-process state — its contract is stable, hard to evolve, and every field needs a serialization decision. EDID parsing is data extraction over inputs every process can read independently from sysfs (no privilege required); pushing that through D-Bus duplicates information for no consistency benefit. The single source of truth that genuinely needs D-Bus is the *matching decision* — which TV is bound to which connector, given the live TV-side `HardwareId` from the daemon's connection. The daemon owns both halves; its result is published on `Tv1.Desktop.Connector` for other tooling. Raw EDID detail doesn't have the same single-source-of-truth need.

### Why not move probe into the library entirely

The matching decision is shared state with multiple consumers (tray, future GUI, future power-management adapters). Without a single source of truth, two consumers re-running the probe could disagree on the binding (especially in same-model multi-TV setups where iteration order matters). Keep the matching outcome on D-Bus, served by the daemon; let the library serve raw EDID parsing to whoever needs it.

### What this means concretely

- `Tv1.Desktop` interface stays minimal: `Connector` only.
- `Tv1.HardwareId` (already there) carries vendor-side identity for icons / pretty names.
- The library extraction is mechanical when GUI work starts (~250 LOC moved); no D-Bus API churn needed at that point.
- An EDID hash field on `DesktopMapping` was considered and rejected — see the discussion in [PR #34](https://github.com/ruffsl/TisVlay/pull/34) and the cull that followed.

## Polkit gating for multi-user shared workstations

Current trust model is "any local user with D-Bus access can mutate config." Fine for single-user desktops (the typical TisVlay deployment); the daemon does the privileged work as the dedicated `tisvlay` system user.

For multi-user shared workstations (lab machines, family PCs where one user's edit shouldn't silently rebind another's TV), polkit is the canonical Linux gate. When this becomes a concern, the work is:

1. Add `polkit::AuthorityProxy::check_authorization` to each privileged `Manager1` / `Tv1` method handler. Read-only properties stay open.
2. Ship `org.tisvlay.policy.xml` declaring action IDs (one per privileged method or grouped by lifecycle).
3. Document the action IDs in the NixOS module so admins can pin the auth rules per their tenancy model.

No GUI-side changes — polkit prompts surface through the existing freedesktop polkit agent (KDE / GNOME both ship one). Adds zero overhead in the single-user case (polkit's `check_authorization` short-circuits when the caller is root or the daemon's own user).

## Profile rename

System and user profiles can be created, edited, deleted today, but not renamed. Rename was scoped out of the M3.5 / M4 profile-CRUD work because it has to walk every TV's `profile.system` / `profile.user` assignment to rewrite atomically — a meaningfully different operation from the per-profile CRUD methods.

Path forward when this becomes a concern:

- Daemon-side `Manager1.RenameSystemProfile(old: s, new: s)` and `Session1.RenameUserProfile(old: s, new: s)` that do the assignment-rewrite under an internal lock.
- Validation: same alphabet rules as create (`[a-z0-9-]+`), `default` undeleteable so also unrenameable.
- Atomicity: load → rewrite all references → save in one disk-write window, same `disk_write_lock` pattern the existing CRUD uses.
- GUI-side: rename input on the profile editor with the same validation feedback the new-profile flow has.

Skipped in v1 simply because no user has asked for it; the create-new-then-reassign-then-delete-old workflow covers the same need with three clicks.

## CLI shorthand for `Tv1.UpdateConfig`

Was scoped as an optional follow-up after the GUI's edit-mode toggle landed. Mirrors NetworkManager's `nmcli connection modify <name|uuid> field value`:

```
tisvlay tvs <name|uuid> set <field>=<value> [<field>=<value>…]
```

Wraps `Tv1.UpdateConfig(json_patch)` so multi-field edits round-trip atomically through the same daemon path the GUI uses. Includes the long-deferred name/UUID dual-lookup for TV identifier args (today the CLI already accepts UUID but not name; this would unify the two).

Cost is mostly clap plumbing — the daemon-side surface is already there. Land when there's a concrete user request or when scripted edits start showing up in operator workflows.

## `a{sv}` wire-format migration for vendor-config surfaces

`Manager1.AddTv` and `Tv1.UpdateConfig` currently take `config_json: s` (JSON string). The architecturally correct end-state per freedesktop convention is `a{sv}` (D-Bus variant dict), matching ModemManager's `Modem.Simple.Connect(properties: a{sv})` and `ProfileManager.Set(properties: a{sv})` shape.

Triggers for reconsidering:

1. A second vendor backend lands and the multi-vendor introspection argument becomes concrete.
2. External tools (third-party clients, monitoring scripts) start consuming the D-Bus surface and the typed introspection becomes user-visible.
3. A vendor config grows nested structure that `a{sv}` would handle awkwardly but JSON would handle cleanly (which would *strengthen* the case for staying on JSON, not weaken it).

Cost of locking in `a{sv}` now is migrating `AddTv` callers during peak-development churn for no concrete user-visible benefit. Cost of staying on JSON is a future wire-flip whose blast radius grows with each shipped consumer. Revisit when the second vendor backend is in flight.

## Atomic config writes (tempfile + rename)

Today's `SystemConfig::save_to` is a synchronous `fs::write` — an interrupted write can leave the on-disk config half-rewritten. The standard Linux pattern is "write to `<path>.tmp`, fsync, `rename(2)` over the live file" so a crash mid-write leaves either the old or the new content but never a torn one.

Scoped out of the Phase 8.1 UUID work because no failure mode has been observed; an interrupted save typically affects a single edit which the user can redo. Worth doing when:

- The first user reports a config that won't load after a crash, or
- A new write site lands that's harder to diagnose post-crash than "rerun the last edit", or
- We add an automated edit pipeline (e.g., GUI batch-edits) where the user wouldn't easily know which edit was in flight.

Implementation is small (~20 LOC: extract a `write_atomic(path, contents)` helper, swap call sites).

## Typed `Failed { reason }` payload across daemon error vocabulary

The `PairingState::Failed` wire format is still `(s)` — a single human-readable string carrying the reason. Plan 5 introduced typed `ConfigError::InvalidVendorConfig { field, reason }` for the pairing-form validation case (so the GUI routes per-field errors back to the originating row), but the broader `Failed { reason }` payload across `Connection` / `Pairing` / `Internal` errors stays untyped on the wire.

Path forward:

- Define typed sub-variants (`Failed::ConnectionRefused`, `Failed::TvUnreachable { last_seen_at }`, `Failed::PairingRejected`, `Failed::Internal { kind }`) under the existing `tisvlay-core/src/errors/` per-domain enum tree.
- D-Bus wire encoding: variant-tagged dict rather than a free-form string, so clients can branch on reason kind.
- Migrate `tisvlay-cli` and the GUI's inline-error surface to render the typed variant rather than the raw string.

Useful when the GUI grows kind-based recovery actions ("Edit MAC", "Open firewall settings", etc.) — each action ID is gated on the corresponding `Failed::*` variant.

## "Open logs" affordance

User asked about a journal-viewer launcher; researched in Plan 5 and dropped because there's no portable `journal://` URI scheme. Options that surfaced:

- **(a)** Spawn `ksystemlog` directly via `xdg-open` — best UX when KSystemLog is installed, degrades on GNOME / minimal installs.
- **(b)** Spawn a terminal with `journalctl -u tisvlayd -f` pre-typed — most accurate but UX is fragile (`xdg-terminal-exec` is freedesktop-spec but spotty).
- **(c)** Copy-to-clipboard a `journalctl` command with a toast saying "Open a terminal and paste" — simplest and degradation-friendly.
- **(d)** Embed a small log viewer inside the GUI (read systemd journal via `systemd::journal::Journal::open` or shell out + parse) — most polished but a sizable engineering chunk.

Leading recommendation: **(a) with (c) as fallback** — try `xdg-open` on the KSystemLog desktop file first; on failure, fall back to copy-to-clipboard with the journalctl command. Defer (d) until a real debugging-support story justifies the engineering cost.

Surface placement: both — About pane gets the always-on shortcut; inline-error messages with `kind == "internal"` get a contextual "View logs" action button.

## DE-driven theming via xdg-desktop-portal Settings

The Slint `fluent` style doesn't pick up the user's KDE accent color or custom palette. True DE-driven theming would require:

- xdg-desktop-portal `Settings` namespace queries for the accent color and color-scheme preference.
- Writing the resolved values into `Palette.accent-background` / `Palette.background` etc. at runtime.
- Watching for Settings-namespace change signals so the UI re-tints when the user changes their KDE settings.

Scoped out of Plan 4's theming sweep (which settled on `fluent` + a stable token surface as the pragmatic v1) because the Settings-namespace work has no user-visible payoff over fluent's defaults until a user complains the GUI looks out-of-place in their themed desktop.

Revisit when a user reports the visual mismatch, or when `cosmic` (the alternative with looser Slint integration) becomes the right default and fluent's hard-coded look starts feeling stale.

## Slint dialog open/close fade animation

Was filed as polish work and deferred — `ConfirmDialog` is so brief in the typical edit/remove flow that a fade-in adds visible lag rather than polish. Revisit if the dialog's role expands (e.g., multi-step removal confirmation, settings-export wizard) and the snappier timing starts feeling abrupt.
