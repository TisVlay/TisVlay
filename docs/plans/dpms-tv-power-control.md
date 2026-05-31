# Add display-blank-driven TV control via a new system-profile event/action pair

## Context

The user's TVs hang off an Nvidia GPU with no HDMI-CEC, so the desktop can't tell the TV "the screen blanked, blank too" the way a CEC-capable setup would. Today `tisvlay-session` approximates this with a session-bus path (ScreenSaver D-Bus + Wayland idle → `Tv1.Power`/`Tv1.Screen`). A prior attempt (PR #34) tried a much broader DE-integration (KWin brightness sync, PipeWire volume sync, an out-of-tree kernel module) and was scrapped as too large.

This time the goal is a **narrow, targeted** addition: one new event/action pair on the **system profile** so a TV reacts to the host display connector's **DPMS** power state (panel blanks → blank the TV; panel wakes → wake the TV), driven entirely **inside `tisvlayd`** with no new per-DE session code. The wire fields are named `on_dpms_off`/`on_dpms_on` — the ecosystem-consensus DPMS state vocabulary (`xset`/`kscreen-doctor`/wlr-protocol all use on/off), pinning the signal source.

**Prior-art finding (web research):** no Linux open-source project does "DPMS → networked TV power" today. The established bridge pattern is an idle manager (swayidle / xss-lock / hypridle) or a CEC bridge (`cec-dpms`) running a script. A daemon-side watcher that reads the EDID-matched connector's `/sys/class/drm/<conn>/dpms` directly — reusing tisvlay's existing connector matching — is novel and fits the "system profile, no per-DE" framing.

**Decisions made with the user:**
- Wire/config field names: `on_dpms_off` / `on_dpms_on`. GUI labels: "On DPMS off" / "On DPMS on".
- Resolved defaults for both = `SessionAction::None` (strictly opt-in; the rationale — avoiding double-dispatch against the existing user-profile idle/lock path — lives in `docs/design.md` + the PR, not a code comment).
- The DPMS path **composes with** the existing `screen_on`/`screen_off` escalation. `screen_on` on an off-ish TV escalates to `power_on` (WoL), so `on_dpms_on = ScreenOn` wakes an off-but-live TV for free. Safety comes from two narrow gates, not from suppressing escalation (see Design).
- If the Wayland sysfs-dpms spike had failed: stop and reassess. It **passed** (below).

---

## Step 0 — Spike (PASSED)

KDE Plasma **Wayland** (`XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=KDE`). `kscreen-doctor --dpms off` → `/sys/class/drm/card1-HDMI-A-1/dpms` read `Off`; `--dpms on` → back to `On`. The legacy sysfs attribute tracks the KWin Wayland DPMS path, so the daemon-only watcher is viable here. `kscreen-doctor --dpms` shares KWin's DPMS code path with the idle-timeout "turn off screen" policy. **Residual caveat to confirm in end-to-end verification:** re-confirm the value flips on a *real idle timeout* too (not just the explicit command).

Land config/dispatch/GUI plumbing **and** the watcher together as one working feature (no dead plumbing) — per the user's "prefer larger chunks" preference.

---

## Design (incorporating review findings)

**Compose with escalation, don't suppress it.** The watcher dispatches the configured `SessionAction` through the existing `screen_on`/`screen_off`/`power_on`/`power_off` manager methods unchanged. `screen_on`/`screen_off` already escalate to `power_on`/WoL for off-ish TVs (`tv_manager.rs:2271`, `2365`) — that is the desired reliability behavior. Two narrow safety gates replace the rejected "connected-only" idea:

1. **Only watch connectors whose sysfs `status == connected`.** A TV that is fully off and has dropped its HDMI/EDID link presents a `disconnected` connector → no edges → no spurious WoL from a stale mapping. Read `status` from the same `cardN-<conn>/` dir as `dpms`.
2. **Skip dispatch when the TV is `reconnecting`.** Avoids kicking off a second WoL while a reconnect/WoL is already in flight (the one genuine race from review). Mirrors the `!tv.reconnecting` guards already used in `resolve_monitored_interface` (`tv_manager.rs:741, 755, 1805`).

**No "idempotent no-op" assumption.** `screen_off`/`screen_on` do *not* skip when already at target — they do an authoritative `refresh_power_state` round-trip + resend (`tv_manager.rs:2267, 2361`). So when both the session idle/lock path and the DPMS path are enabled on the same TV they double-dispatch convergent commands (extra traffic, not corruption). This is acceptable because the resolved default is `None` (opt-in); the core `screen_*` methods are **not** modified. Documented, not silently assumed away.

**Per-TV scoped dispatch.** A blank on TV-A's connector must not act on TV-B. `match_displays_to_tvs` claims each connector for at most one TV (`desktop_probe.rs` greedy `claimed` set), so connector→TV is 1:1. Add `execute_system_event_for_tv(tv_id, event, context)` and reimplement the existing `execute_system_event` as a loop over it. (Note: an HDMI splitter presents one EDID on one host connector, so only one of two TVs behind a splitter can be matched/driven — do not imply both react.)

**Off-tick dispatch.** The poll tick does only cheap sysfs reads + edge detection and updates `EdgeDetector` state synchronously; each dispatch is `tokio::spawn`'d (fire-and-forget, like `netlink`/subscription tasks) so a slow `screen_on` (seconds, with WoL escalation) doesn't stall polling of other connectors or re-fire the same edge. Use `tokio::time::interval` with `MissedTickBehavior::Skip` (not the default burst-catch-up).

---

## Implementation

Field order in code is `on_startup, on_shutdown, on_reboot, on_no_session, on_sleep, on_wake, automate_when`; insert the two new fields after `on_wake`, before `automate_when`.

### A. Core config — `crates/tisvlay-core/src/config.rs`

Add `on_dpms_off` / `on_dpms_on: Option<SessionAction>` (with `#[serde(default, skip_serializing_if = "Option::is_none")]`) at every site:

1. `struct SystemProfileConfig`.
2. `struct ResolvedSystemProfile` (non-`Option`).
3. `impl Default for ResolvedSystemProfile` — both `SessionAction::None` (no rationale comment; it lives in design.md).
4. `full_default()` — `Some(d.on_dpms_off)` / `Some(d.on_dpms_on)`.
5. `merge_with_base()` — `self.x.or(base.x)`.
6. `resolve()` — `self.x.unwrap_or(d.x)`.
7. `SystemProfileBody` (`#[cfg(feature = "dbus")]`) + `from_config()` + `From<&SystemProfileBody>`.

Doc comment on the new fields: one line, behavioral ("Action when the matched host display connector blanks / wakes"), no default value restated.

### B. Daemon dispatch — `crates/tisvlayd/src/tv_manager.rs`

1. `enum SystemEvent` — add `DpmsOff`, `DpmsOn`. Update the enum's doc line to name the watcher as a third source (source-agnostic wording).
2. `get_system_action()` match — add the two arms (compiler-enforced; exhaustive match).
3. Add `pub async fn execute_system_event_for_tv(&self, tv_id, event, context)`; reimplement `execute_system_event` as a loop over `list_automated_tv_ids()` calling it. Extract `is_tv_automated(tv_id)` from the existing gate (`tv_manager.rs:828-838`). In the per-TV method, also skip when `reconnecting` (gate 2 above).
4. The resolved-view body constructor in **`get_effective_system_profile`** (`tv_manager.rs:1164`, builds a `SystemProfileBody` from a `ResolvedSystemProfile`) — add both fields.
5. `execute_system_event`'s `SessionAction` match needs **no** change (exhaustive).

### C. New watcher — `crates/tisvlayd/src/dpms.rs` (+ `pub mod dpms;` in `lib.rs`)

Naming mirrors `netlink.rs` (`spawn_link_monitor` / `watch_link_carrier`) and the injected-root testability of `desktop_probe.rs` (`enumerate_at`):

- `DpmsState { On, Blanked }` — `On` vs everything else (Off/Standby/Suspend collapse to `Blanked`).
- `parse_dpms(&str) -> DpmsState`.
- `resolve_dpms_path(root, connector) -> Option<PathBuf>` — bare connector (`HDMI-A-1`) → `cardN-HDMI-A-1` via suffix match with a `-` boundary guard (do not re-document the full `cardN-` scheme; it lives in `desktop_probe.rs`). Read both `status` and `dpms` from the resolved dir; treat `status != "connected"` as "not watchable" (gate 1).
- `EdgeDetector` — per-connector last-state; first observation returns `None` (no boot-time spurious edge); `On→Blanked` → `DpmsOff`, `Blanked→On` → `DpmsOn`; `retain(active)` drops vanished connectors.
- `poll_once(manager, root, edges)` — iterate `manager.list_tv_ids()` → `get_desktop_mapping(tv_id)`; skip empty connector or `status != connected`; on an edge, `tokio::spawn` the scoped `execute_system_event_for_tv(tv_id, event, "display")` (off-tick) after updating `EdgeDetector` state.
- `spawn_dpms_monitor(manager: Arc<TvManager>)` → `watch_dpms(manager, Path::new("/sys/class/drm"), POLL_INTERVAL)` loop. `POLL_INTERVAL = Duration::from_secs(1)`, `MissedTickBehavior::Skip`. Unreadable `/sys/class/drm` → no connectors resolved, harmless no-op loop (matches `enumerate_at`).

Wire the spawn in `crates/tisvlayd/src/main.rs` near the other monitors, passing a cloned `Arc<TvManager>` (note: `netlink::spawn_link_monitor` takes `state_change_sender()`, not the Arc — the DPMS watcher needs the Arc to call `list_tv_ids`/`get_desktop_mapping`/`execute_system_event_for_tv`). Spawn after the state forwarder is up. Not gated on `--session` (host-display feature, bus-independent).

### D. GUI — `crates/tisvlay-gui/src/profiles.rs`

The GUI reuses core's `SystemProfileBody`; the profile pane is fully data-driven (`profiles_pane.slint` iterates `Profiles.editor-fields`) — **no `.slint` change**.

1. `SYSTEM_FIELDS` — two `FieldSpec` enum entries copying `on_wake` (`SESSION_ACTION_OPTIONS`, `EnumClass::SessionAction`), after `on_wake`.
2. `field_label()` — `gettext("On DPMS off")` / `gettext("On DPMS on")` (has a fall-through arm, so add explicitly).
3. `read_system_field()` — read both via `session_action_token`.
4. `set_system_field()` (~line 1110) — patch both via `session_action_patch`.

### E. Tests

- **`config.rs`:** extend the all-fields TOML parse test (`test_parse_profile_config_all_fields`, ~2273); fix the exhaustive no-`..Default` literals that won't compile — `system_profile_body_round_trip_full` (2702), `system_profile_body_from_config_preserves_options` (2759) — and assert both new fields round-trip; add `full_default`/`merge_with_base`/`resolve` assertions mirroring `on_sleep`.
- **`dpms.rs`:** `#[cfg(test)] mod tests` (inline, like `netlink.rs`/`desktop_probe.rs`) over a `tempfile::tempdir()` fake `cardN-CONN/{status,dpms}` tree (`tempfile` already a tisvlayd dev-dep): table-test `parse_dpms`; suffix-boundary `resolve_dpms_path`; `status != connected` skip; `EdgeDetector` first-obs/blank/wake/repeat/`retain`; `poll_once` flipping the file and asserting the scoped action fired.
- **`profiles.rs`:** fix `baseline_system()` (1504) no-spread literal; add the two wires to `system_field_table_covers_every_body_field` (1705, **exact-count** assert) and `set_system_field_writes_every_session_action_wire` (1847).

### F. Docs, i18n, meta

- `docs/configs/daemon.toml` — add `on_dpms_off` / `on_dpms_on` to the reserved-default profile with an effect-describing comment (not `(default: none)`).
- `docs/design.md` — add the fields to both the `SystemProfileBody` Rust listing and the TOML schema block (same position as code); add `on_dpms_off`/`on_dpms_on` rows to the power-management decisions table (~629) carrying the `None`/opt-in/overlap rationale.
- `docs/ideas/event-driven-power-management.md` — reword the "Don't ship sysfs-DPMS polling as the primary path" item to scope the prohibition to the *fast lockscreen-snap-back primary path*; this is an *opt-in, system-profile, secondary* path that doesn't touch that requirement.
- `.claude/CLAUDE.md` — add `dpms.rs` to the project-structure tree; note in "What's Next" that the DPMS system-profile slice shipped; add a "Known Quirks" entry for the Wayland sysfs-dpms finding (legacy `dpms` attr tracks KWin Wayland blanking).
- `just i18n-extract` to refresh the `.pot` (hygiene; not a CI gate). CLI needs no change.
- **On merge (closeout):** migrate decisions → `design.md`, the Wayland-sysfs caveat → Known Quirks, then delete this plan file, drop its `docs/plans/README.md` entry, and `grep -rn 'docs/plans/dpms-tv-power-control'` to re-point any references (per `docs/plans/README.md` convention).

---

## Dependency order

Step 0 (done) → A (core types) → B (daemon SystemEvent + per-TV dispatch + `get_effective_system_profile` body) → C (watcher + spawn) → D (GUI) → E (fix no-spread literals in the same change, then add coverage) → F (docs + `just i18n-extract`).

## Verification (end-to-end)

1. `nix develop -c just check` — pipe once to a file and grep (per workflow prefs).
2. Real daemon: set a TV's `on_dpms_off`/`on_dpms_on` (GUI Profiles pane or `busctl` system-profile write), blank/wake the panel via a **real idle timeout**, confirm the TV blanks/wakes within ~1 s. Cross-check `busctl --user monitor org.tisvlay.Daemon`.
3. Confirm a network-only TV (no matched connector) and a disconnected connector are untouched; confirm the existing session idle/lock blanking still works (idempotent overlap, no regression).
4. `prek run -a` before handoff.

## Risks / call-outs

- **Real-idle-timeout sysfs flip** — spike used the explicit command; confirm idle-policy blanking also flips `dpms` (verification step 2).
- **Escalation semantic** — a remote-turned-off TV can be re-woken by a panel blank→wake cycle when `on_dpms_on` is set (opt-in; documented).
- **First-obs swallows a pre-first-tick blank** — a blank that happens before the connector first appears in the mapping is missed until the next wake edge; acceptable for v1, doc line.
- **Multi-GPU `cardN` ambiguity** — same connector base name on two cards could mis-resolve; systemic to the matcher too, low likelihood on target, note-and-defer.
