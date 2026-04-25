# Plan: Relocate Desktop Probe from `tisvlay-session` to `tisvlayd`

Status: **planning**, scoped before squash-merge of `dev` → `main`. Drafted as a recall point because context is near auto-compact.

## Background

The cull merge currently on dev (squash target for this PR — see [PR.md](../PR.md)) introduces a DRM-EDID desktop probe that matches managed TVs to physical DRM connectors via `HardwareId`. The probe lives in `crates/tisvlay-session/src/desktop_probe.rs` because of historical co-location with the (now-removed) brightness/volume sync coordinators that needed session-bus access. With sync gone, that rationale evaporated and the probe should live in `tisvlayd` instead — daemon-side ownership is the correct architecture and we should ship it that way from the squash to main.

Reasons daemon-side is correct:

- **Single source of truth.** Multi-user systems with two logged-in graphical sessions today would have both push `Manager1.SetDesktopMapping`; last-write-wins, race-prone. Daemon-side probing eliminates the cross-process state push entirely.
- **Clean TV lifecycle integration.** Daemon already owns the TV list and `HardwareId`s; matching naturally runs alongside TV-add and TV-connect events.
- **Drops `Manager1.SetDesktopMapping` from the D-Bus surface.** No external pusher, so no public API surface for it.
- **Robust under sandboxed sessions.** Containerized / Flatpak'd user sessions can have `/sys/class/drm` masked. Daemon as a system service always has the unisolated view.
- **No privilege barrier.** `/sys/class/drm/*/edid` is world-readable on standard Linux; the daemon's existing systemd hardening (`ProtectSystem=strict`, `PrivateDevices=true`, `ProtectKernelTunables=true`, etc.) doesn't block sysfs reads — those settings only restrict `/dev`, `/proc/sys` writes, and `/sys/kernel` writes.

## Scope

### Files to move

- `crates/tisvlay-session/src/desktop_probe.rs` → `crates/tisvlayd/src/desktop_probe.rs`. The module is already structured as library-friendly code (pure `enumerate_at`, `match_displays_to_tvs`, `parse_hardware_id`, `identity_matches`, `match_score`); the move is mechanical.

### Files to delete

- `tisvlay-session/src/lib.rs::probe_and_push_desktop_mappings` — the probe call from the session crate goes away entirely.

### D-Bus surface changes

- **Drop `Manager1.SetDesktopMapping(tv_id, mapping)`.** No external pusher, no public method.
  - In [`tisvlayd/src/dbus/manager.rs`](../crates/tisvlayd/src/dbus/manager.rs).
  - In [`tisvlay-core/src/dbus_client.rs`](../crates/tisvlay-core/src/dbus_client.rs) — drop the `set_desktop_mapping` trait method on `TisvlayManager` proxy and the `DaemonClient::set_desktop_mapping` wrapper.

- **Keep `Tv1.Desktop.Connector` property on the daemon.** That's the matching outcome readers consume; that's the API surface that survives.

### Daemon trigger points

Pick **startup + `Manager1.ReloadConfig`** for parity with today's session-side behavior. udev hotplug for live DRM connector changes is a follow-up that fits naturally into the event-driven power-management redesign in [docs/ideas.md](ideas.md).

Concrete wiring:

- `tisvlayd/src/main.rs` — after `TvManager` is built and before the D-Bus connection serves traffic, run a one-shot probe + match for all TVs that have a non-empty `HardwareId`. Populate `TvManager`'s desktop-mapping cache via the existing internal `set_desktop_mapping(tv_id, mapping)` method.
  - Note that `HardwareId` is populated by the LG backend on first SSAP connection. At daemon startup before any TV has connected, every TV's `HardwareId` will be empty, so the initial probe is essentially a no-op. The first useful probe runs on `Manager1.ReloadConfig` (after the session/tray triggers it post-connect) OR — better — gets re-triggered by the daemon itself on each `ConnectionChanged(true)` event in `state_change_forwarder`. Decide between:
    - **A: Re-probe on `ConnectionChanged(true)`** — every TV connect re-runs the global probe + match. Simple, captures the moment `HardwareId` becomes available for that TV. The greedy claim is global so order-dependence between two simultaneous connects is theoretical only (channel is serial).
    - **B: Re-probe on `Manager1.ReloadConfig`** — only.
  - Recommend **A**. It's the cleanest "match runs when matching is possible" semantic; ReloadConfig still triggers it as a side-effect because the connection forwarder fires on the event chain.

- `tisvlayd/src/dbus/manager.rs::reload_config` — also call the probe in case the TV list changed (added/removed TVs invalidate prior matches).

### Greedy-claim re-probe semantics

The matcher is `(displays, tvs) → assignment`, sorted by connector name + greedy claim. Re-running it on each `ConnectionChanged(true)` is fine — it's deterministic over the same inputs. If the TV set changes (e.g., one TV temporarily disconnects), the claim re-runs. Two scenarios:

- TV A connects first, claims HDMI-A-1. Then TV B connects, also claims HDMI-A-1 (would beat A on score). Re-probe at TV B's connect re-evaluates globally: B gets HDMI-A-1, A gets HDMI-A-2 (or unmatched). The cache for A flips. Tooling consumers see a `Tv1.Desktop.Connector` invalidates signal on both TVs and re-read.

This is correct behavior — better than today's session-side single-shot at startup (which freezes whichever TV connected first into its initial assignment).

### Test strategy

Probe unit tests already exist (`enumerate_at`, `match_displays_to_tvs`, `identity_matches`, `match_score`, deterministic-claim regressions). They follow the file move; no new tests required.

New integration test (optional): TestDaemon variant that exercises the on-connect probe path — possibly out of scope for the move. Current snapshot tests on `TvInfo.desktop` should pick up the new mapping after `MockHandle::set_hardware_id` triggers a re-probe.

### Cargo dependencies

- Move `edidkit = "0.1"` from `tisvlay-session/Cargo.toml` to `tisvlayd/Cargo.toml`. Currently a direct (non-workspace) dep on the session side.
- Verify `cargo machete` reports no orphans in either crate after the move.

## Verification

- `just check` clean (246 tests + clippy + fmt).
- `cargo machete` clean.
- `nix flake check` clean.
- Hardware drill: pair an LG TV, observe `busctl --system get-property org.tisvlay.Daemon /org/tisvlay/Tv/<id> org.tisvlay.Tv1.Desktop Connector` returns the matched DRM connector name within ~1 second of TV connect (down from "after session restart" today).
- D-Bus introspection: `Manager1.SetDesktopMapping` no longer appears in `busctl --system introspect org.tisvlay.Daemon /org/tisvlay/Manager`.

## Out of scope

- udev hotplug listener for DRM connector changes — fits the event-driven power-management redesign in [docs/ideas.md](ideas.md). When that lands, the same listener can re-trigger the desktop probe.
- Library extraction of probe code into a reusable crate (per [docs/ideas.md](ideas.md) "Probe library extraction (when GUI lands)"). The probe is daemon-internal post-relocation; library extraction is the next step when the GUI starts consuming it directly.

## Follow-up edits to merge artifacts

After the relocation lands, update before squash-merging:

- [PR.md](../PR.md) — what's-new table:
  - "DRM probe + EDID-based TV ↔ connector matching" row updates from `tisvlay-session/src/desktop_probe.rs` → `tisvlayd/src/desktop_probe.rs`
  - "`Manager1.SetDesktopMapping`" row deletes (no longer in the public surface)
  - "Verification" diff-grep predicate adds `Manager1.SetDesktopMapping` to the must-not-appear list
- [docs/ideas.md](ideas.md) — "Probe library extraction" section:
  - Tweak the framing: probe is now in tisvlayd, not the session crate. Library extraction is from tisvlayd → reusable lib when GUI lands.
- [docs/design.md](design.md) — `Manager1` method list:
  - Drop `SetDesktopMapping` from the methods enumeration.
- This file ([docs/plan.md](plan.md)) — delete after the relocation lands and the squash-merge proceeds. The plan is forensic-only.

## Sequence of commits

Suggested commit boundary (single focused commit):

1. Move file (`git mv`).
2. Update tisvlayd's `lib.rs` + `main.rs` to declare and call the module.
3. Wire probe trigger into `state_change_forwarder` for `ConnectionChanged(true)`.
4. Drop `Manager1.SetDesktopMapping` D-Bus method + dbus_client proxy + DaemonClient wrapper.
5. Drop `probe_and_push_desktop_mappings` from session crate's `lib.rs`.
6. Move `edidkit` dep.
7. Update PR.md, ideas.md, design.md per the follow-up list above.
8. Delete this plan.md.
9. `just check` + commit.

Approximate diff: ~150 LOC moved (file relocation), ~50 LOC net removed (D-Bus method + session push code + dbus_client trait method), tests follow.
