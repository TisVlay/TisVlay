# Add DPMS-driven TV power control via a new system-profile event/action pair

## Context

The user's TVs hang off an Nvidia GPU with no HDMI-CEC, so the desktop can't tell the TV "the screen blanked, blank too" the way a CEC-capable setup would. Today `tisvlay-session` approximates this with a session-bus path (ScreenSaver D-Bus + Wayland idle → `Tv1.Power`/`Tv1.Screen`). A prior attempt (PR #34) tried a much broader DE-integration (KWin brightness sync, PipeWire volume sync, an out-of-tree kernel module) and was scrapped as too large.

This time the goal is a **narrow, targeted** addition: one new event/action pair on the **system profile** so a TV reacts to the host display connector's **DPMS** state (panel blanks → blank the TV; panel wakes → wake the TV), driven entirely **inside `tisvlayd`** with no new per-DE session code.

**Prior-art finding (web research):** no Linux open-source project does "DPMS → networked TV power" today. The established bridge pattern is an idle manager (swayidle / xss-lock / hypridle) or a CEC bridge (`cec-dpms`) running a script. A daemon-side watcher that reads the EDID-matched connector's `/sys/class/drm/<conn>/dpms` directly — reusing tisvlay's existing connector matching — is novel and fits the "system profile, no per-DE" framing.

**Make-or-break unknown (gates everything):** the `dpms` sysfs attribute exists and reads `On` at rest, but it is **unverified** that KWin on **Wayland** writes its DPMS-off through to that legacy attribute (Xorg historically does). If it doesn't, the daemon-only approach is non-viable on the user's KDE Plasma Wayland desktop.

**Decisions made with the user:**
- Resolved defaults for both new actions = `SessionAction::None` (strictly opt-in; avoids double-firing `ScreenOff` alongside the existing user-profile idle/lock path).
- If the Wayland spike fails: **stop, report, and reassess together** — do *not* auto-pivot to a compositor signal or ship X11-only.

---

## Step 0 — Spike first (blocks all implementation)

Verify the legacy sysfs `dpms` attribute tracks compositor blanking on the target host:

```bash
# baseline
cat /sys/class/drm/card1-HDMI-A-1/dpms        # -> On

# blank the panel via the compositor (pick what works on KDE Wayland):
#   kscreen-doctor --dpms off   (or)   loginctl lock-session / idle wait
cat /sys/class/drm/card1-HDMI-A-1/dpms        # need this to flip to Off/Standby
```

- **If it flips to a non-`On` value on blank and back to `On` on wake → proceed** to the implementation steps below.
- **If it stays `On` under Wayland → STOP.** Report the finding; do not write the watcher. (Per user decision: pause and reassess the event-source strategy.)

Per the user's "prefer larger chunks" preference, once the spike passes, land the config/dispatch/GUI plumbing **and** the watcher together as one working feature (no dead plumbing).

---

## Implementation (only after Step 0 passes)

### A. Core config — `crates/tisvlay-core/src/config.rs`

Add `on_dpms_blank` / `on_dpms_wake: Option<SessionAction>` by copying the existing `on_wake` sibling verbatim at **every** site (each must be hit or the field silently fails to resolve):

1. `struct SystemProfileConfig` — two `Option` fields with `#[serde(default, skip_serializing_if = "Option::is_none")]`, placed after `on_wake`, before `automate_when`.
2. `struct ResolvedSystemProfile` — two non-`Option` fields.
3. `impl Default for ResolvedSystemProfile` — **both `SessionAction::None`** (the opt-in decision).
4. `SystemProfileConfig::full_default()` — `Some(d.on_dpms_blank)` / `Some(d.on_dpms_wake)`.
5. `merge_with_base()` — `self.x.or(base.x)`.
6. `resolve()` — `self.x.unwrap_or(d.x)`.
7. `struct SystemProfileBody` (D-Bus wire, `#[cfg(feature = "dbus")]`) + `SystemProfileBody::from_config()` + `From<&SystemProfileBody> for SystemProfileConfig` — carry both fields through.

The `SessionAction { None, ScreenOff, ScreenOn, PowerOff, PowerOn }` vocabulary is complete — **no action changes**.

### B. Daemon dispatch — `crates/tisvlayd/src/tv_manager.rs`

1. `enum SystemEvent` — add `DpmsBlank`, `DpmsWake`.
2. `get_system_action()` match — add `DpmsBlank => tv.system_profile.on_dpms_blank`, `DpmsWake => tv.system_profile.on_dpms_wake`.
3. Add **per-TV scoped** dispatch `pub async fn execute_system_event_for_tv(&self, tv_id, event, context)` — the watcher knows exactly which TV's connector changed; a global `execute_system_event` would wrongly act on TVs whose connector did *not* change. Extract a small `is_tv_automated(tv_id)` helper from the existing `list_automated_tv_ids()` gate logic and reuse the existing `screen_off/on`, `power_off/on` arms + logging. Reimplement the existing `execute_system_event` as a loop calling the new per-TV method (one dispatch definition).
4. The resolved-view `SystemProfileBody` constructor (~`tv_manager.rs:1164`, the `GetSystemProfile` path) — add `on_dpms_blank: Some(resolved.on_dpms_blank)`, `on_dpms_wake: Some(resolved.on_dpms_wake)`.
5. `execute_system_event`'s `SessionAction` match needs **no change** (already exhaustive).

### C. New watcher — `crates/tisvlayd/src/dpms_watch.rs` (+ `pub mod dpms_watch;` in `lib.rs`)

Pattern after `desktop_probe.rs` (injected-root testability) and `netlink.rs` (`spawn_*` background task). Pure, unit-testable helpers + a thin async loop:

- `parse_dpms(&str) -> DpmsState` — `On` vs `Blanked` (Off/Standby/Suspend all collapse to `Blanked`).
- `resolve_dpms_path(root, connector) -> Option<PathBuf>` — the mapping stores the bare connector (`HDMI-A-1`); the sysfs dir is `cardN-HDMI-A-1`. Suffix-match with a `-` boundary guard so `HDMI-A-1` does not match `card1-HDMI-A-12`.
- `EdgeDetector` — per-connector last-state map. First observation returns `None` (no spurious edge at boot); `On→Blanked` → `DpmsBlank`; `Blanked→On` → `DpmsWake`; `retain(active)` drops connectors no longer mapped (detached/re-probed away).
- `poll_once(manager, root, edges)` — iterate `manager.list_tv_ids()` → `get_desktop_mapping(tv_id)`; skip TVs with no/empty connector (the detached/network-only case, free); on an edge call `manager.execute_system_event_for_tv(tv_id, event, "dpms")`. Re-reading mappings every tick picks up hotplug/re-probe for free (kept current by `state_forwarder::refresh_desktop_mapping`).
- `spawn_dpms_watch(Arc<TvManager>)` — `tokio::time::interval(POLL_INTERVAL)` loop over real `/sys/class/drm`. **`POLL_INTERVAL = Duration::from_secs(1)`** (module const, no config knob in v1): the sub-100 ms snap-back requirement applies to the *lockscreen* path, not DPMS; ~1 s wake latency is imperceptible against the TV's own SSAP screen-on ramp. Unreadable `/sys/class/drm` (sandbox) → no connectors resolved, harmless no-op loop (same degradation as `enumerate_at`).

Wire the spawn in `crates/tisvlayd/src/main.rs` next to `netlink::spawn_link_monitor`, cloning `shared_manager`. Do **not** gate on `--session` (DPMS is a host-display feature independent of the bus).

**Overlap with the session path is acceptable:** on KDE both may fire `ScreenOff`; actions pre-flight-check `last_known_power_state`, so redundant commands are cheap idempotent no-ops. No cross-path suppression in v1.

### D. GUI — `crates/tisvlay-gui/src/profiles.rs`

The GUI reuses core's `SystemProfileBody` (no separate GUI body type).

1. `SYSTEM_FIELDS` — two `FieldSpec` entries copying `on_wake` (`FieldKind::Enum { SESSION_ACTION_OPTIONS, EnumClass::SessionAction }`), after `on_wake`, before `automate_when`.
2. `field_label()` — `gettext("On display blank")` / `gettext("On display wake")` (plainer than "DPMS"; matches the "On sleep"/"On wake" neighbors; both **must** be `gettext()`-wrapped).
3. `read_system_field()` — read both via `session_action_token`.
4. `set_system_field()` (the write/dispatch arm, ~line 1115) — patch both via `session_action_patch`.

### E. Tests

- **`config.rs`:** extend the all-fields TOML parse test (`on_dpms_blank = "screen_off"` etc.); fix the no-`..Default` struct literals that will fail to compile — `system_profile_body_round_trip_full` (~2702) and `system_profile_body_from_config_preserves_options` (~2759) — and assert the new fields survive round-trip; add `full_default`/`merge_with_base`/`resolve` assertions mirroring `on_sleep`.
- **`dpms_watch.rs`:** `#[cfg(test)] mod tests` over a `tempfile::tempdir()` fake `cardN-CONN/dpms` tree (writable, unlike `/sys`): table-test `parse_dpms`; suffix-boundary `resolve_dpms_path`; `EdgeDetector` first-obs/blank/wake/repeat/`retain`; a `poll_once` end-to-end flipping the file and asserting the scoped action fired.
- **`profiles.rs`:** fix `baseline_system()` (~1504) no-spread literal; add the two wires to `system_field_table_covers_every_body_field` (exact-count assert) and `set_system_field_writes_every_session_action_wire`.

### F. Docs & i18n

- `docs/configs/daemon.toml` — add `on_dpms_blank = "none"` / `on_dpms_wake = "none"` to the reserved-default profile.
- `docs/design.md` — add the fields to both the `SystemProfileBody` Rust listing and the TOML schema block, with a one-line comment.
- `docs/ideas/event-driven-power-management.md` — note that the daemon DPMS sysfs-poll path is now the realized narrow slice (and the Wayland-sysfs caveat).
- Run `just i18n-extract` to refresh the `.pot` with the two new msgids (English-only; no manual `.po` edits). CLI needs no change (it only assigns profiles by UUID, doesn't enumerate fields).

---

## Dependency order

Step 0 spike → A (core types) → B (daemon SystemEvent + per-TV dispatch + resolved-view body) → C (watcher + spawn) → D (GUI) → E (fix no-spread test literals in the same change so the build stays green, then add coverage) → F (docs + `just i18n-extract`).

## Verification (end-to-end)

1. `nix develop -c just check` (fmt + build + clippy + test) — pipe once to a file and grep, per workflow prefs.
2. Manual on the real daemon: set a TV's `on_dpms_blank`/`on_dpms_wake` (via GUI Profiles pane or `busctl` system-profile write), then blank/wake the panel and confirm the TV blanks/wakes within ~1 s. Cross-check with `busctl --user monitor org.tisvlay.Daemon`.
3. Confirm a TV with **no** matched connector (network-only) is untouched by the DPMS path, and that the existing session idle/lock blanking still works (no regression, idempotent overlap).
4. `prek run -a` before handing off for commit.

## Risks / call-outs

- **Wayland sysfs-dpms viability (Step 0)** — the gate. Pause-and-reassess if it fails.
- **X11 vs Wayland divergence** — feature may end up Xorg-strong / Wayland-dependent; document whatever Step 0 shows.
- **`SystemEvent::DpmsBlank/DpmsWake` dead-code** — only briefly, since the watcher (C) is landed in the same change; no `#[allow(dead_code)]` needed if shipped together.
- **Multi-GPU `cardN` ambiguity** — same connector base name on two cards could mis-resolve; low likelihood on the single-TV-per-connector target, noted as a known edge.
