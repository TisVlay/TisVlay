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

`idle_timeout_seconds` (session) also defaults to `0` = disabled. Note both profiles carry an `on_idle_timeout`/`on_idle_resume` pair — system = *greeter* idle, user = *in-session* idle — driven by two independent idle clocks.

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
- **Presence collapses many events into one state.** Greeter idle/resume, session lock/unlock, in-session idle/resume, greeter-wake — all are "presence changed." The configured action (`ScreenOff`/`ScreenOn`) sets a **persistent per-TV desired screen** (latest-wins, latching).
- **Reconcile, don't fire.** A per-TV reconcile drives the panel toward the desired screen whenever the TV is ready, and re-asserts it after reconnect/boot. Crucially, **`on_startup` powers the TV on but does *not* force the screen on** — the presence layer (greeter attended vs idle) owns the screen. That one change resolves both the cold-boot override (A2) and the autologin dark screen (C2).

This is a *declarative* refactor of the existing event→action wiring, not a new policy: the actions still come from config, but they set state that is reconciled rather than commands that fire and are lost.

## Scope decisions to make (need your call)

1. **Adopt the two-axes / persistent-desired-screen model**, or keep the imperative model and bolt latching + readiness-gating onto just the greeter path?
2. **Should `on_startup`/`on_wake` stop forcing screen-on** and let the presence layer own the screen? (The elegant fix for A2/C2 — but a behavior change worth stating loudly.)
3. **Unify the two idle pipelines** (greeter system-idle + in-session user-idle) under one presence abstraction, or leave them parallel and put the desired-state layer beneath both?
4. **This PR vs split.** `feat/power-default` is already large. Land the whole model here, or land the model doc + the cold-boot (A3) and cross-login (C2) fixes now, with the full unification as a follow-up?
5. **Multi-TV.** Desired-state is obviously per-TV; do we tackle heterogeneous idle timeouts now, or keep today's "min non-zero" pragmatic caveat?

## Non-goals (draft)

- Multi-seat / fast-user-switching.
- Replacing PowerDevil — we mirror/augment its in-session DPMS, not supplant it.
- Input/HDR-specific screen policy; non-LG presence nuances.

## Pointers

- Rejected narrow approach + why: [`coalesce-deferred-screen-actions.md`](coalesce-deferred-screen-actions.md).
- Broader per-DE power redesign this feeds into: [`../ideas/event-driven-power-management.md`](../ideas/event-driven-power-management.md).
