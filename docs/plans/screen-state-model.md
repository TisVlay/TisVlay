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
7. **Explicit control is sticky and high-precedence** — the screen equivalent of `powered_off` (J1).
8. **Keep every event configurable.** Events still map to *configurable* profile actions; the model makes the configured action set a reconciled desired-state instead of firing once — it does not hardcode a presence→screen policy. (Honors the standing "every event maps to a configurable action" rule.)

## Proposed organizing principle (for discussion)

A single idea covers most of the stories: **presence drives a persistent, reconciled desired screen state; the OS lifecycle drives TV power separately.**

- **Two axes.** *Power* (TV reachable / warming) and *screen* (panel lit) are independent. The OLED-safe idle state is "powered on, screen off." OS-lifecycle events (`on_startup`/`on_wake`/`on_sleep`/`on_shutdown`) own power and stay imperative/immediate.
- **Presence collapses many events into one state.** Greeter idle/resume, session lock/unlock, in-session idle/resume, lock-wake — all are "presence changed." The configured action (`ScreenOff`/`ScreenOn`) sets a **persistent per-TV desired screen** (latest-wins, latching).
- **Reconcile, don't fire.** A per-TV reconcile drives the panel toward the desired screen whenever the TV is ready, and re-asserts it after reconnect/boot. Crucially, **`on_startup` powers the TV on but does *not* force the screen on** — the presence layer (greeter attended vs idle) owns the screen. That one change resolves both the cold-boot override (A2) and the autologin dark screen (C2).

This is a *declarative* refactor of the existing event→action wiring, not a new policy: the actions still come from config, but they set state that is reconciled rather than commands that fire and are lost.

## Scope — resolved (2026-06-13)

- **Wayland-native.** X11 is best-effort / no guarantees, no commitment (the GUI already dropped X11). Design to the Wayland protocols.
- **Don't assume PowerDevil.** It is KDE-specific; other Wayland DEs have no equivalent, so the in-session idle timer is load-bearing there. The model must not silently depend on PowerDevil to blank an idle session.
- **Consequence — fix in-session idle.** Move the in-session idle pair off the dead `GetSessionIdleTime` poll onto `ext-idle-notify-v1` (the path the greeter already uses), so it actually fires on Wayland without PowerDevil.
- **Breadth — the desired-state layer covers both greeter *and* session** (not greeter-only). The earlier "broken-parts-only" lean is superseded: leaving in-session to PowerDevil would re-introduce the hidden KDE dependency above.
- **Greeter = `plasma-login-manager` for now.** Generalizing to other Wayland greeters (if any gain traction) is a post-release, contributor-driven follow-up.
- **Rename `on_greeter_wake` → `on_lock_wake`** (user profile; exact spelling provisional). Clean trio: `on_session_lock` (off) → `on_lock_wake` (on, to show the prompt) → `on_session_unlock` (on). No back-compat shim.

## Model shape — resolved (2026-06-13)

Confirmed with the maintainer: **full two-axes desired-state model, event-driven**, with the **login presence signal in the daemon (logind)**. `on_startup`'s panel-wake is *gated on presence* rather than removed (see below — a freshly-WoL'd LG is `ActiveStandby`/dark, so the wake is load-bearing). Multi-TV heterogeneous idle timeouts keep today's "min non-zero" caveat (low priority; one TV = the desktop is exact).

## Implementation plan

### Model mechanics

Per-TV **`desired_screen: Option<bool>`** on `ManagedTv` (`None` = no standing opinion this boot; resets each daemon start). It is the single source of truth for the panel; the *power* axis (`power_on`/`power_off`) is untouched and immediate.

**Writers (all latest-wins):**

- **Presence events** — greeter idle/resume, session lock/unlock/lock-wake, in-session idle/resume, and the new login signal — set `Some(bool)` via their configured action. They no longer issue an imperative `screen_off`/`screen_on` with its own pre-flight; they set `desired_screen` and trigger a reconcile. (This removes the A3 cold-boot pre-flight tangle.)
- **`on_startup`** — after power-on, wake the panel (`screen_on`, which escalates an `ActiveStandby` panel to `Active`) **only if `desired_screen != Some(false)`**. A freshly-WoL'd LG lands in `ActiveStandby` (panel dark) before `Active` ([tv_manager.rs:1262](../../crates/tisvlayd/src/tv_manager.rs), `:2666-2691`), so the wake is load-bearing for the attended greeter (A1) — but it defers to a presence "off" already recorded by an away user's greeter-idle (A2).
- **Explicit user** (GUI/CLI `SetScreen`) — sets `Some(bool)`, latest-wins (J1: stays until the next presence event supersedes it).

**Reconcile (reader):** `reconcile_screen(self: &Arc<Self>, tv_id)` reads `desired_screen` + observed `PowerState` under the per-TV lock; if `desired_screen` is `Some` and differs from the panel, it applies the raw screen command. It is **triggered on transition to a ready state**, not on connection-success — a per-TV watcher on the existing `power_state_tx` ([tv_manager.rs:355](../../crates/tisvlayd/src/tv_manager.rs), sent at `:637`/`:2483`/`:2504`) spawned alongside the subscription (`refresh_subscription`, holding `Arc<TvManager>` like `state_poll_loop`). Triggering on state→ready (not power_on-success) plus idempotent re-application is what prevents the rejected plan's `reconcile→screen_off→power_on→reconcile` WoL storm.

**Why `&Arc<Self>`:** `power_on`/`reconnect` are `&self` (and bound by the `ActionExecutor` trait), so reconcile is *not* called from inside them; the `power_state_tx` watcher (holds `Arc<TvManager>`) calls `reconcile_screen`. `on_startup`/`reconnect_tvs_on_interface` already use the `&Arc<Self>` pattern.

### Commit breakdown

1. **Core wayland — multiple idle notifications.** `tisvlay-core/src/wayland.rs` `_idle_notification` is a single `Option` (`:49`,`:255`); extend to N notifications at different timeouts on one connection (ext-idle-notify-v1 allows it). Prereq for #2.
2. **Session idle → ext-idle.** Replace `idle.rs`'s dead `GetSessionIdleTime` poll with an ext-idle notification at `idle_timeout_seconds`, on the same connection as the 200 ms greeter-wake (`wayland_idle.rs:24`,`:37`). `on_idle_timeout`/`on_idle_resume` then fire on Wayland without PowerDevil. Delete the `GetSessionIdleTime` path.
3. **Daemon `desired_screen` + reconcile.** Add the field to `ManagedTv`; add `reconcile_screen`; spawn the `power_state_tx`→ready watcher in `refresh_subscription`; route presence dispatch (greeter `NotifyGreeterIdle`, session events) through `desired_screen` instead of direct `screen_on/off`; non-escalating, idempotent reconcile.
4. **`on_startup`/`on_wake` gating.** Gate `on_startup`'s `screen_on` (`:1262`) on `desired_screen != Some(false)`; `on_wake` drives power only.
5. **Login presence signal (daemon).** Add a `SessionNew` handler to `logind.rs` (none today; pattern at `:127-179`); for a new *active graphical user* session (query session `Type`/`Class`/`Active` to exclude the plasmalogin greeter session), set `desired_screen=on` for automated TVs. Fixes autologin C2 + logout/resume.
6. **Rename `on_greeter_wake` → `on_lock_wake`.** ~30 sites: `config.rs` (`:407`,`:430`,`:453`,`:467`,`:569` + tests), GUI `profiles.rs` (`:209`,`:256`,`:439`,`:1171` + tests + `.pot`), session (`SessionEvent::GreeterWake`→`LockWake` at `session_monitor.rs:23`,`:38`; `wayland_idle.rs`; `lib.rs:266`,`:496`; `session_dbus.rs:265`), sample configs (`docs/configs/*.toml`), docs (`design.md`, `README.md`). No back-compat shim.

### Hard parts / risks (for review)

- **Reconcile vs `ActiveStandby` escalation.** Applying `desired=on` to an `ActiveStandby` panel needs `screen_on`'s pre-flight wake; applying `desired=off` to a not-ready TV is the momentary-full-brightness-flash case. The state→ready trigger + idempotency must converge without re-escalation loops. **This is the delicate core.**
- **`on_startup` gating race.** Greeter-idle setting `desired=off` vs `on_startup`'s gate check at ~T+85 s; latest-wins must make either order end correctly (away→off, attended→on).
- **Multi-notification wayland refactor** touches the shared transport the greeter binary also uses — keep greeter behavior identical.
- **logind session-type discrimination** (user vs greeter) is new; must not fire the login signal for the plasmalogin greeter session itself.
- **Reconcile-watcher lifecycle** — spawned/dropped with the subscription; no leak, no double-fire.

### Tests

mock + `tisvlay-harness`: (a) presence `off` while reconnecting → deferred, applied on ready; (b) off-then-on while not ready → only `on` (latest-wins); (c) `on_startup` defers to a pre-recorded greeter `desired=off` (A2); (d) attended boot → panel on (A1); (e) login signal → `on` (C2); (f) session ext-idle drives `IdleTimeout`/`IdleResume`. Drive readiness via the mock's `set_connect_delay` / `inject_power_state`.

### Verification

`just check`; hardware: cold boot attended (panel on) and away (blanks, *stays* off through the on_startup window), autologin (panel on), suspend/resume, full lock cycle.

## Non-goals

- X11 support (best-effort only; no guarantees, no commitment).
- Other Wayland greeters beyond `plasma-login-manager` (revisit post-release if contributors want them).
- Multi-seat / fast-user-switching.
- Replacing PowerDevil — on KDE we still mirror its DPMS-off OLED-safe; we just stop *depending* on it for the in-session idle blank.
- Input/HDR-specific screen policy; non-LG presence nuances.

## Pointers

- Rejected narrow approach + why: [`coalesce-deferred-screen-actions.md`](coalesce-deferred-screen-actions.md).
- Broader per-DE power redesign this feeds into: [`../ideas/event-driven-power-management.md`](../ideas/event-driven-power-management.md).
