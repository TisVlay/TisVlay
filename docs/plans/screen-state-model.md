# Screen & power state model — user stories and scope

## Why this doc

The narrow "coalesce deferred screen actions" plan was rejected in review (see [`coalesce-deferred-screen-actions.md`](coalesce-deferred-screen-actions.md)): it didn't fire in its own headline scenario, recursed on reconnect, and only patched one slice. The deeper problem the review exposed is that the daemon has **no persistent notion of what the screen should be**. Every presence/lifecycle event fires an *imperative* screen command that can be lost (TV not ready), tangled (pre-flight power-on during boot), or overridden (`on_startup`'s screen-on beats an away user's blank). This doc maps the user stories across every boot/login/lock/idle/sleep context to fix the **goal and scope** before designing the mechanism. Design comes after we agree on these.

## Frame: one display, one owner, the TV *is* the desktop

Target hardware is a single Nvidia/KDE workstation whose display is an LG OLED (no HDMI-CEC; control is network/SSAP). The owner is in exactly one *presence state* at a time. OLED burn-in is the cost of getting it wrong. So the two things the owner cares about:

- **Protect the panel:** whenever no one is present (idle at greeter, locked, idle in session, away), the panel goes to a true OLED-safe screen-off — and *stays* off until someone is actually present again.
- **Show the screen:** whenever the owner is present or arriving (keypress, login, unlock, resume), the panel is on — promptly, without flicker, double-wakes, or WoL storms.

## The contexts (presence timeline)

| Phase | Owner present? | Screen should be | TV power should be |
|---|---|---|---|
| Cold boot → greeter | arriving / maybe | on if attended; OLED-off if not | on (reachable) |
| Greeter, attended | yes | on | on |
| Greeter, unattended (idle) | **no** | **OLED-off, and stays off** | on |
| Login — interactive (typed password) | yes | on | on |
| Login — autologin / zero-input | yes (will be) | on | on |
| Active session | yes | on | on |
| Session locked | no | OLED-off | on |
| Lock screen, waked (keypress) | yes | on | on |
| Session idle, unlocked | no | OLED-off | on |
| Logout → greeter | maybe | follows greeter presence | on |
| Suspend | n/a | off | off |
| Resume | arriving | on (or lock-screen path) | on |
| Shutdown | n/a | off | off |
| Reboot | n/a | unchanged (boots again) | unchanged |
| Explicit user "screen off" | n/a | off, sticky until a real event | per command |
| Explicit user "TV off" | n/a | off | off, sticky (no auto-reconnect) |

## Signals available today (ground truth)

**System profile (daemon).** Events and their resolved core defaults:

| Event | Trigger | Action field | Core default |
|---|---|---|---|
| Startup | first boot (marker file) | `on_startup` | `PowerOn` |
| Shutdown | logind `PrepareForShutdown` (poweroff/halt) | `on_shutdown` | `PowerOff` |
| Reboot | logind `PrepareForShutdown` (reboot/kexec) | `on_reboot` | `None` |
| Sleep | logind `PrepareForSleep(true)` | `on_sleep` | `PowerOff` |
| Wake | logind `PrepareForSleep(false)` | `on_wake` | `PowerOn` |
| DpmsOff | DPMS sysfs poll `On→Blanked` | `on_dpms_off` | `ScreenOff` *(flipped this PR)* |
| DpmsOn | DPMS sysfs poll `Blanked→On` | `on_dpms_on` | `ScreenOn` *(flipped this PR)* |
| GreeterIdle | greeter client `NotifyGreeterIdle(true)` | `on_idle_timeout` | `ScreenOff` |
| GreeterResume | greeter client `NotifyGreeterIdle(false)` | `on_idle_resume` | `ScreenOn` |

`idle_timeout_seconds` (greeter) defaults to `0` = disabled; both greeter rows are gated on it being `> 0`.

**User profile (session).** Events and their resolved core defaults:

| Event | Trigger | Action field | Core default |
|---|---|---|---|
| SessionLocked | `ScreenSaver.ActiveChanged(true)` | `on_session_lock` | `ScreenOff` |
| SessionUnlocked | `ScreenSaver.ActiveChanged(false)` | `on_session_unlock` | `ScreenOn` |
| GreeterWake | ext-idle `Resumed` while locked (once/lock cycle) | `on_greeter_wake` | `ScreenOn` |
| IdleTimeout | idle ≥ timeout | `on_idle_timeout` | `ScreenOff` |
| IdleResume | activity after idle | `on_idle_resume` | `ScreenOn` |

`idle_timeout_seconds` (session) also defaults to `0` = disabled. Both profiles carry an `on_idle_timeout`/`on_idle_resume` pair — system = *greeter* idle, user = *in-session* idle — on two independent clocks. **Mechanism caveat:** the greeter idle uses `ext-idle-notify-v1` (works), but the in-session idle pair still uses the D-Bus `GetSessionIdleTime` poll, which the project flags as non-functional on KDE Plasma Wayland ([wayland.rs](../../crates/tisvlay-core/src/wayland.rs)). So on a KDE Wayland box the in-session `on_idle_timeout`/`on_idle_resume` effectively never fire — PowerDevil's DPMS timeout blanks the idle unlocked session and the daemon only mirrors it. **Naming caveat:** `on_greeter_wake` (user profile) fires at the in-session *lock screen* (first input while locked, via ext-idle), not the logged-out greeter — the real greeter wake is the *system* profile's `on_idle_resume`.

**Readiness model.** A TV is `connected` (live handle) and has a `PowerState` (`Active`, `ScreenSaver`, `ScreenOff` = ready; `Standby`/`ActiveStandby`/`Suspend`/`WarmingUp` = not ready). `needs_wake_preflight` is true for any not-ready state, so `screen_off`/`screen_on` power the TV fully on first. The `reconnecting` flag is set by `power_on`/`reconnect` but **not** by `on_startup`'s power-on (it calls `wake_and_connect_tv` directly). A cold LG takes ~85 s to reach `Active`.

## User stories

Status legend: ✓ works today · ⚠ partial / fragile · ✗ broken.

### A — Cold boot
- **A1.** When the system powers on, I want the TV on and showing the greeter, so I can log in. — ✓ `on_startup=PowerOn` wakes + `screen_on` (~85 s).
- **A2.** When I boot but no one attends the greeter, I want the panel to go OLED-off after the idle timeout **and stay off**, so the login screen can't burn in. — ✗ Even if the greeter idle blanks it, `on_startup`'s `screen_on` (or the greeter-idle firing *during* the wake window) overrides; nothing latches "off".
- **A3.** When the greeter idle fires *during* the cold-boot wake, I want no double-wake, no full-brightness flash, no WoL storm — just the correct end state. — ✗ The greeter `screen_off` runs its own pre-flight `power_on`, racing `on_startup`'s in-flight wake.

### B — At the greeter
- **B1.** Unattended ≥ timeout → panel OLED-off, stays off. — ⚠ Works only if the timeout out-runs cold boot *and* nothing re-asserts on; no latching.
- **B2.** I return and press a key / move the mouse → panel on promptly. — ✓ greeter `Resumed` → `screen_on`.
- **B3.** I press Esc (manual blank) → true OLED panel-off, not a lit "No Signal" banner. — ✓ DPMS mirror `on_dpms_off=ScreenOff` (this PR).

### C — Login transition (greeter → session)
- **C1.** I type my password → panel stays on through login. — ✓ the keypress is a greeter `Resumed` → `screen_on` before auth; the in-flight command survives the greeter exiting.
- **C2.** Autologin / zero-input login while the greeter had blanked the panel → panel comes on for my session. — ✗ greeter sends no resume on exit; the session asserts nothing on a normal (daemon-available) start → **dark screen after login**.
- **C3.** Login while the panel was already on → stays on, no flicker. — ⚠ likely fine; depends on no spurious re-dispatch.

### D — Active session
- **D1.** While I'm using it, the panel never surprise-blanks. — ⚠ generally fine; in-session DPMS mirror + session events can double-dispatch (idempotent, but a reconcile model should make this explicit).
- **D2.** PowerDevil's own DPMS/idle keeps working; tisvlay reflects it OLED-safely. — ✓ redundant-but-safe mirror.

### E — Session lock
- **E1.** Lock (manual/auto) → panel OLED-off. — ✓ `on_session_lock=ScreenOff`.
- **E2.** Locked and idle → stays off. — ✓.
- **E3.** Wake at the lock screen (keypress) → panel on to show the prompt. — ✓ `on_greeter_wake=ScreenOn`, once per lock cycle.
- **E4.** Unlock → panel on. — ✓ `on_session_unlock=ScreenOn`.

### F — Session idle (unlocked)
- **F1.** Idle ≥ timeout → panel OLED-off. — ✓ when enabled (`on_idle_timeout=ScreenOff`, gate default off).
- **F2.** Resume → panel on. — ✓ `on_idle_resume=ScreenOn`.

### G — Logout (session → greeter)
- **G1.** I log out → the greeter returns with the panel on (or follows greeter presence). — ⚠ symmetric to C2: nothing asserts screen-on when the greeter re-appears; relies on the greeter not having idled.

### H — Suspend / resume
- **H1.** Suspend → TV powers off. — ✓ `on_sleep=PowerOff`.
- **H2.** Resume → TV powers on; screen follows what's shown (lock screen vs session). — ⚠ `on_wake=PowerOn` (sets `reconnecting`); the screen state after resume depends on the lock path firing, not on an asserted desired state.

### I — Shutdown / reboot
- **I1.** Shutdown → TV powers off. — ✓ `on_shutdown=PowerOff`.
- **I2.** Reboot → TV left as-is (it boots again shortly). — ✓ `on_reboot=None`.

### J — Explicit control (GUI / CLI / tray)
- **J1.** I set the screen off → it stays off until a real presence/lifecycle event, not undone by a stale deferred action. — ⚠ no sticky screen intent today (only power is sticky).
- **J2.** I power the TV off → it stays off, not auto-reconnected. — ✓ `powered_off` sticky flag.

### K — Reconnect / network (orthogonal)
- **K1.** NIC failover / carrier blip → the TV reconnects without spuriously changing my screen state or losing it. — ⚠ no desired-state to re-assert after reconnect.

## What the stories tell us (derived requirements)

1. **Persistent desired screen state, per TV — a single source of truth.** Today screen control is imperative and stateless; that's the root of A2/A3/B1/C2/G1/J1/K1.
2. **Latching.** The desired screen persists until a *real* presence or explicit-control change — surviving `on_startup`, reconnect, and cross-login handoffs (fixes A2, C2, G1, K1).
3. **Readiness-gated reconcile.** Apply the desired screen when the TV is ready (connected + Active); while not ready, record-and-defer rather than fire-and-tangle (fixes A3; no pre-flight storm).
4. **Separate the power axis from the screen axis.** OS-lifecycle events drive TV *power* (immediate); presence/control events drive the *screen* (reconciled). Today `on_startup` couples them (power-on **and** force screen-on), which is exactly what overrides the away-user blank in A2.
5. **Cross-login continuity.** The desired screen carries across greeter↔session handoffs so neither autologin (C2) nor logout (G1) drops to a dark/over-lit panel.
6. **Idempotent, no-flicker, no-recursion.** Re-applying the current state is a no-op; reconcile must not re-enter the `screen→power_on→reconcile` loop (the rejected plan's WoL storm).
7. **Explicit control is latest-wins, never stale-clobbered** (J1). A manual screen-off is the current truth until the next *presence* event supersedes it — it must not be undone by a *stale/deferred* command, but (decided 2026-06-13) it does **not** outrank future presence: screen-off is a weaker statement than `powered_off`, which stays sticky.
8. **Keep every event configurable.** Events still map to *configurable* profile actions; the model makes the configured action set a reconciled desired-state instead of firing once — it does not hardcode a presence→screen policy. (Honors the standing "every event maps to a configurable action" rule.)

## Organizing principle

A single idea covers most of the stories: **presence drives a persistent, reconciled desired screen state; the OS lifecycle drives TV power separately.**

- **Two axes.** *Power* (TV reachable / warming) and *screen* (panel lit) are independent. The OLED-safe idle state is "powered on, screen off." OS-lifecycle events (`on_startup`/`on_wake`/`on_sleep`/`on_shutdown`) own power and stay imperative/immediate.
- **Presence collapses many events into one state.** Greeter idle/resume, session lock/unlock, in-session idle/resume, lock-wake — all are "presence changed." The configured action (`ScreenOff`/`ScreenOn`) sets a **persistent per-TV desired screen** (latest-wins, latching).
- **Reconcile, don't fire.** A per-TV reconcile drives the panel toward the desired screen on every connection/state change, and re-asserts it after reconnect/boot. `on_startup` powers the TV on and wakes the panel **only when presence hasn't already said off** (a WoL'd LG is `ActiveStandby`/dark, so the wake is load-bearing) — presence (greeter attended vs idle) otherwise owns the screen. That resolves both the cold-boot override (A2) and the autologin dark screen (C2).

This is a *declarative* refactor of the existing event→action wiring, not a new policy: the actions still come from config, but they set state that is reconciled rather than commands that fire and are lost.

## Scope — resolved (2026-06-13)

- **Wayland-native.** X11 is best-effort / no guarantees, no commitment (the GUI already dropped X11). Design to the Wayland protocols.
- **Don't assume PowerDevil.** It is KDE-specific; other Wayland DEs have no equivalent, so the in-session idle timer is load-bearing there. The model must not silently depend on PowerDevil to blank an idle session.
- **Consequence — fix in-session idle.** Move the in-session idle pair off the dead `GetSessionIdleTime` poll onto `ext-idle-notify-v1` (the path the greeter already uses), so it actually fires on Wayland without PowerDevil.
- **Breadth — the desired-state layer covers both greeter *and* session** (not greeter-only). The earlier "broken-parts-only" lean is superseded: leaving in-session to PowerDevil would re-introduce the hidden KDE dependency above.
- **Greeter = `plasma-login-manager` for now.** Generalizing to other Wayland greeters (if any gain traction) is a post-release, contributor-driven follow-up.
- **Rename `on_greeter_wake` → `on_lock_wake`** (user profile; exact spelling provisional). Clean trio: `on_session_lock` (off) → `on_lock_wake` (on, to show the prompt) → `on_session_unlock` (on). No back-compat shim.

## Model shape — resolved (2026-06-13)

Confirmed with the maintainer: **full two-axes desired-state model, event-driven**; the **login presence signal in the daemon (logind)**; explicit screen control is **latest-wins** (a manual screen-off is superseded by the next presence event — no provenance/sticky); shipped as a **single PR** (#83) with a coherent commit sequence (splitting strands cases — see below). `on_startup`'s panel-wake is *gated on presence*, not removed (a WoL'd LG is `ActiveStandby`/dark, so the wake is load-bearing). Multi-TV heterogeneous idle timeouts keep today's "min non-zero" caveat (already per-TV-correct via `get_system_action`).

> **Revised after three design/impl/maintainability reviews.** The first cut — "reconcile on transition to a *ready* state," reusing `screen_on`/`screen_off` — was unsound on four counts: (1) a post-WoL `ActiveStandby` panel is never "ready" and never auto-promotes, so reconcile never fired; (2) `screen_on`/`screen_off` both escalate to `power_on`, reviving the WoL-storm recursion; (3) session presence is out-of-process and reaches the daemon only as `SetScreen`, so it can't set `desired_screen` without rerouting that handler; (4) the latch strands logout/resume/restart. All four are fixed below.

## Implementation plan

### Model mechanics

Per-TV **`desired_screen: Option<bool>`** on `ManagedTv` (`None` = no opinion yet; resets each daemon start). Single source of truth for the panel; **latest-wins** — the most recent intent from any source is the truth. The *power* axis (`power_on`/`power_off`, `powered_off`) is untouched and immediate.

**Intent writers** (set `desired_screen`, then call `reconcile_screen`):

- The daemon's **`screen_on`/`screen_off`** — and therefore `Tv1.Screen.SetScreen` and `apply_action(ScreenOn|ScreenOff)` — are rewritten to **set `desired_screen` + reconcile**, not drive the panel directly. This is the key structural change: every screen caller becomes a latching writer through one path — GUI/CLI/tray, **the session process** (lock/unlock/lock-wake/idle, which reach the daemon only as `SetScreen` over D-Bus — fixes critical #3), the greeter (`NotifyGreeterIdle`), the DPMS mirror, and the new login/resume/greeter-present signals.
- **`on_startup`** (PowerOn branch) already did `wake_and_connect` (the power axis); it then sets `desired_screen = Some(true)` **only if not already `Some(false)`** and reconciles (driving the panel) — so the attended greeter is woken (A1) but an away user's already-recorded greeter `desired=off` is honored (A2).

**`reconcile_screen(&self, tv_id)`** — the *only* path that touches the panel, via two internal escalation-free primitives `apply_screen_on_raw`/`apply_screen_off_raw` (the current `screen_on`/`screen_off` bodies **minus** the `power_on` pre-flight and tail escalations; the projector `WarmingUp`/`CoolingDown` branches and verify loops are kept). It reads `desired_screen` + the cached `PowerState` under the per-TV lock, drops the lock (the `tokio::Mutex` is non-reentrant and `with_connection` re-locks), then:

| `desired_screen` | cached state | action |
|---|---|---|
| `Some(true)` | `Active`/`ScreenSaver` | no-op (already on) |
| `Some(true)` | anything else | `apply_screen_on_raw` (bare `turnOnScreen` + verify) |
| `Some(false)` | `ScreenOff` | no-op (already off) |
| `Some(false)` | anything else | `apply_screen_off_raw` (bare `turnOffScreen` + verify) |
| `None` | any | no-op |

**Reconcile never calls `power_on`** — that is the recursion fix (refined after the focused review, which showed *any* single `reconcile_in_flight` flag either re-storms WoL or never lights the screen, because the post-wake push can be `ActiveStandby` *or* `Unknown`, both `needs_wake_preflight`). Power lives purely on the lifecycle axis: `on_startup`'s `wake_and_connect` and `on_wake`'s `power_on` bring the TV up; reconcile then drives only the panel via the bare, idempotent commands (LG masks the "already on/off" 500). With no `power_on` inside reconcile, the `screen→power_on→state-push→reconcile` loop is impossible *by construction* — no flag, no cooldown. (Residual, flagged for hardware verify: if `turnOnScreen` alone can't wake a stuck `ActiveStandby` panel, attended-boot panel-on relies on the TV already being `Active` after `wake_and_connect` — the common cold-boot case.)

**Reconcile trigger:** one manager-wide channel, not per-TV tasks. `forward_state_snapshot` (which already runs on every push *and* poll, for every TV including live-added ones) sends the `tv_id` after it updates `power_state_tx`; a single `reconcile_loop(Arc<TvManager>)` spawned once at startup (the `state_poll_loop` precedent) drains it and calls `reconcile_screen`. One spawn site, no per-TV task lifecycle/leak (the review showed per-TV-task-at-registration needs three spawn sites + teardown bookkeeping). Intent writers also call `reconcile_screen` directly, and the public `screen_on`/`screen_off` **await** it so the CLI/`SetScreen` "done" contract holds.

### Presence completeness — re-assert signals (fixes critical #4)

The latch that fixes A2/C2/K1 strands G1/H/daemon-restart unless presence is re-asserted on those transitions. Each sets `desired=on` for automated TVs (latest-wins — an immediately-following lock re-blanks):

- **Login** — daemon `logind` `SessionNew` matching `Class=user ∧ Type∈{wayland,x11} ∧ Active ∧ seat=seat0` (excludes the plasmalogin greeter *and* SSH/polkit/user-manager sessions). Fixes C2 autologin.
- **Resume** — in `handle_prepare_for_sleep(false)`, after the `on_wake` power action. Fixes H resume-to-unlocked/blanked.
- **Greeter present** — `tisvlay-greeter` asserts present once at startup (`NotifyGreeterIdle(false)` before the idle watch). Fixes G1 logout→greeter.
- **Daemon restart** — `connect_all_on_restart` re-establishes intent per TV (present unless the session reports locked), since `on_startup` early-returns on a restart (boot marker present).

### Commit sequence (single PR #83)

One PR — splitting would land a half-built model that strands G1/H/restart (cruft). Internally sequenced, each commit compiling + green:

1. **Rename `on_greeter_wake` → `on_lock_wake`** (~60 occurrences / 14 files incl. the `deny_unknown_fields` wire contract + `session_dbus_contract.rs:150` in lockstep). Mechanical, first, so later diffs read in final vocabulary.
2. **Wayland multi-notification + tagging** — `wayland.rs` `_idle_notification` (`:49`,`:255`) → a tagged collection; change the `Dispatch` userdata from `()` (`:114`) to a `NotificationKind` so the consumer routes 200 ms (`LockWake`) vs `idle_timeout_seconds` (`IdleTimeout`/`IdleResume`).
3. **Session idle → ext-idle** — replace `idle.rs`'s dead `GetSessionIdleTime` poll with a second notification on the existing connection; delete the poll path. In-session idle now fires on Wayland without PowerDevil.
4. **`desired_screen` + `reconcile_screen` + `apply_screen_raw` + the reconcile task**; rewrite `screen_on`/`screen_off`/`SetScreen`/`apply_action` to set-intent+reconcile; gate `on_startup`'s wake; `on_wake` power-only.
5. **Re-assert signals** — login (`SessionNew`), resume, greeter-present-on-start, restart (one shared "assert present unless locked" helper).

### Hard parts / risks (for review)

- **The reconcile decision table + `reconcile_in_flight` guard** is the delicate core — the guarded wake (desired=on + dark) must terminate (one wake; post-wake push either reaches `Active`→bare-apply or stays dark→no re-wake while in-flight). Test the `ActiveStandby`→wake→`Active` path explicitly.
- **`on_startup` gate atomicity** — read `desired_screen` and conditionally set `Some(true)` in one lock acquisition, then reconcile; a greeter `desired=off` arriving mid-window must not be lost.
- **Login predicate** — the full `Class/Type/Active/seat` filter; a non-graphical `SessionNew` (SSH/polkit/user-manager) must **not** fire (negative test).
- **Multi-notification** touches the greeter's shared transport — greeter-unchanged regression.
- **Reconcile-task lifecycle** — spawned once per TV at registration; torn down on `Tv1.Remove`; no leak, no double-fire.

### Tests

mock + `tisvlay-harness` (drive readiness via `set_connect_delay`/`inject_power_state`, `profile_dispatch.rs` template): (a) presence `off` while not-ready → deferred, applied on next state push; (b) off-then-on while not-ready → only `on` (latest-wins); (c) `on_startup` defers to a pre-set greeter `desired=off` (A2 — fixture must set `idle_timeout_seconds>0`, else inert); (d) attended boot → panel on (A1); (e) `ActiveStandby` + desired=on → guarded wake → `Active`, single command (no storm); (f) login `SessionNew` (graphical, seat0) → on; **(f′) non-graphical `SessionNew` → no action**; (g) resume → on; (h) greeter-present-on-start → on; (i) daemon-restart-mid-session re-establishes intent; (j) session ext-idle drives `IdleTimeout`/`IdleResume`.

### Verification

`just check`; hardware: cold boot attended (panel on) and away (blanks, *stays* off through the on_startup window — with a configured greeter timeout), autologin (panel on), logout→greeter (on), suspend/resume (no stranded dark panel), full lock cycle, manual `SetScreen off` then unlock (comes on — latest-wins).

## Non-goals

- X11 support (best-effort only; no guarantees, no commitment).
- Other Wayland greeters beyond `plasma-login-manager` (revisit post-release if contributors want them).
- Multi-seat / fast-user-switching.
- Replacing PowerDevil — on KDE we still mirror its DPMS-off OLED-safe; we just stop *depending* on it for the in-session idle blank.
- Input/HDR-specific screen policy; non-LG presence nuances.

## Pointers

- Rejected narrow approach + why: [`coalesce-deferred-screen-actions.md`](coalesce-deferred-screen-actions.md).
- Broader per-DE power redesign this feeds into: [`../ideas/event-driven-power-management.md`](../ideas/event-driven-power-management.md).
