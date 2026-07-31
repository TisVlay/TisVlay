# Session that boots already-locked never triggers the idle screen-off

**Priority**: medium
**Found by**: field report on the C3 (LG OLED42C3PUA), NixOS + KDE Plasma 6 Wayland
**Status**: symptom reproduced from archived boot journald; root cause narrowed, not yet confirmed

## Summary

On a desktop that boots straight into a locked session (KDE `LockOnStart=true`, greeter-to-locked handoff, autologin-then-lock), idling on the boot lock screen never fires the configured **locked** idle timeout. The TV stays on until some *other* event (the compositor eventually dropping the connector to DPMS off) turns it off on a much longer schedule. A manual unlock→lock cycle drives the TV correctly, so the lock/unlock *transition* path is healthy.

> Note: the original field report inferred that TisVlay "treats the session as unlocked because there was no lock edge, so the locked-idle path is never armed." **Inspecting the archived boot journald refutes that** — the locked-idle watch *was* armed at boot. See [Root cause](#root-cause-corrected) below. The corrected finding points upstream of TisVlay, at whether the compositor delivers `ext-idle-notify` idle while the screen is locked.

## Environment

- **TisVlay:** 0.3.0
- **OS:** NixOS (flake `services.tisvlay` module), `cec.enable = true`, `cec.pulse8.enable = true`
- **Desktop:** KDE Plasma 6 on Wayland (KWin), `ext-idle-notify` v2 server
- **TV:** LG OLED42C3PUA — reachable both over the network (`<lg>` / `wss`) and via HDMI-CEC (`<cec>`, `/dev/cec0`), matched to DRM connector `card1-HDMI-A-1`
- **Lock config** (`~/.config/kscreenlockerrc`):
  ```ini
  [Daemon]
  Autolock=false
  LockOnStart=true
  Timeout=0
  ```
- **Idle config** (`~/.config/tisvlay/session.toml`):
  ```toml
  [profile.body]
  name = "default"
  locked_idle_timeout_seconds = 5
  unlocked_idle_timeout_seconds = 300
  ```

## Expected behavior

After booting into a locked session and idling past `locked_idle_timeout_seconds` (5s), TisVlay turns the matched TV's screen off — the same action it correctly performs after a live lock transition.

## Actual behavior

Nothing fires from the idle timeout while sitting on the boot lock screen. The TV only turns off much later, when the HDMI connector independently transitions to `dpms=Off` and TisVlay mirrors that to the TV (`activestandby`, `is_screen_on:false`) — i.e. on the compositor's schedule, not the 5s idle timeout.

## Evidence (archived boot journald)

Reconstructed from the boot drive's journal (boot `0`, system up 2026-07-30 10:48:55), read read-only with `journalctl -D …/var/log/journal/<machine-id>`.

| # | Finding | Evidence |
|---|---------|----------|
| 1 | Booted straight into `kscreenlocker` (confirms `LockOnStart`) | `kscreenlocker_greet` running at 10:49:05 |
| 2 | The session armed **all three** idle watches at boot — lock-wake (200 ms), locked-idle (5 s), unlocked-idle (300 s) | `Wayland idle watch started` with `F_COUNT=3` at 10:49:06 |
| 3 | The watch registered once and never churned across the 9h50m session | single `started` line, zero `Wayland idle watch stopped` lines |
| 4 | No session event, screen, or power rpc fired the entire day | nothing at INFO between 10:49:06 and shutdown |
| 5 | The lock watcher never hit its bus-failure path | zero `lock watcher: system bus` warnings |
| 6 | Debug/trace was not captured — the session ran at INFO | priority histogram shows only 4/5/6, **no PRIORITY=7** |

`F_COUNT=3` is the load-bearing fact: it means TV meta had already hydrated and `recompute_idle` had resolved `(5, 300)` *before* the idle adapter registered, so the 5 s locked-idle watch was live against KWin's `ext-idle-notify` for the whole session — yet no idle-driven action ever took effect.

logind reported the session locked but never idle, the entire time:

```
$ loginctl show-session 1 -p LockedHint -p IdleHint -p IdleSinceHint
LockedHint=yes
IdleHint=no
IdleSinceHint=0
```

The TV state in TisVlay's own event store (`tisvlay watch`) only ever changed via the DPMS mirror, never via a 5 s idle action:

```
tv/<lg>/meta/identity  {… "connector":"card1-HDMI-A-1" … "vendor":"lg"}
tv/<lg>/state/power    {"is_on":true,"is_screen_on":false,"state":"activestandby"}
```

## Root cause (corrected)

The failure is **upstream of TisVlay's dispatch**, not in its lock-state handling.

The locked-idle rule reads the lock flag *live* at dispatch ([`idle_dispatch`](../../crates/tisvlay-session/src/lib.rs), `(LOCKED_IDLE_TAG, Idled) if is_locked`), and the lock watcher seeds `is_locked` from `LockedHint` at startup ([`lock.rs`](../../crates/tisvlay-session/src/lock.rs), silent first-attach seed). Neither needs a lock *edge*. Evidence #2 confirms the watch was armed with the correct 5 s timeout. So the "no lock edge → never armed" theory is refuted.

Since a correctly-armed watch produced no action all day, two hypotheses remain — and they are constrained against each other:

- **(A) KWin does not deliver `ext-idle-notify` idle while the session is locked.** A live 5 s watch that never yields an `Idled` transition points here.
- **(B) `is_locked` was not seeded true, so the `Idled` was gated out by the `if is_locked` guard.** But (B) *alone* is insufficient: with `is_locked == false`, the 300 s unlocked watch would have fired `IdleTimeout` at ~10:54 and turned the TV off — which also never happened. So (B) only holds if idle transitions were not being delivered at all.

Both roads converge on **no actionable `Idled` transition reaching the session while locked**. The original "isolation test" (`loginctl unlock-session && lock-session`) only exercised lock/unlock *transitions*; it never tested idle-*while*-locked, so there is in fact no evidence that path ever worked on this machine. The eventual DPMS-off does not contradict this — PowerDevil's "turn off screen" uses KWin's internal DPMS path, not necessarily the same client-facing `ext-idle-notify` stream.

Evidence #6 is why the archived logs cannot close the A-vs-B fork: the session ran at INFO, so the `debug!("session event")` dispatch line and the `trace!("Wayland idle transition")` seam were never written.

## Risk

Any boot-into-locked configuration (a very common KDE/greeter setup) leaves an OLED panel lit on the login/lock screen for the full DPMS timeout instead of the intended few seconds — the exact burn-in exposure the idle protection exists to prevent.

## Mitigation options

- **Confirm A vs B with a DEBUG repro boot first** (see [Reproducer](#reproducer)) — do not implement the original "synthetic lock event at startup" fix, which would change nothing since the watch is already armed.
- If **(B)** (seed race/failure): harden the ordering so the `is_locked` seed is observed before the first idle can dispatch, or re-check `LockedHint` on the first `Idled` rather than trusting the cached flag.
- If **(A)** (compositor withholds idle while locked): the wayland transport cannot fix it. Options are a lock-driven timer session-side (arm a locked-idle deadline off the `Lock`/seed itself rather than off `ext-idle-notify`), or a different idle source — though logind `IdleHint` is also `no` here, so that is not a drop-in substitute.

## Workarounds (config-side, no code change)

- Set `Autolock=true` (with a short idle) in `kscreenlockerrc` so a genuine idle→lock **transition** occurs after login, or
- Emit `loginctl lock-session` from a user-session oneshot shortly after login to synthesize the edge.

## Reproducer

A single DEBUG boot resolves the A-vs-B fork. With the session unit running at:

```
RUST_LOG=tisvlay_session=debug,tisvlay_core=trace
```

boot into the locked session, sit idle past 5 s, then grep the journal for `Wayland idle transition` and `session event`:

- **transitions present, `is_locked=false`** → hypothesis B (seed race) — worth hardening session-side.
- **no transitions at all** → hypothesis A (KWin withholds idle while locked) — TisVlay-side robustness cannot fix it; a lock-driven timer or alternate idle source is required.

## Re-evaluation triggers

- A DEBUG repro boot is captured (resolves A vs B).
- Any change to the KWin / `ext-idle-notify` idle-while-locked behavior lands upstream.
- The idle adapter or lock watcher is refactored for another reason.

## Possibly related / separate

The **managed CEC device** on `/dev/cec0` fails its power-on handshake (`Backend error (cec): TV did not report power-on after repeated Image View On`), while the LG network backend works. Likely a distinct issue, noted here only for context.
