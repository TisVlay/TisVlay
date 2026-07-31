# Session that boots already-locked never triggers the idle screen-off

**Priority**: medium
**Found by**: field report on the C3 (LG OLED42C3PUA), NixOS + KDE Plasma 6 Wayland
**Status**: compositor ruled out by a live `swayidle` probe; root cause identified as an `is_locked` startup seed race in tisvlay-session; fix is a synchronous seed before the idle adapter starts

## Summary

On a desktop that boots straight into a locked session (KDE `LockOnStart=true`, greeter-to-locked handoff, autologin-then-lock), idling on the boot lock screen never fires the configured **locked** idle timeout. The TV stays on until some *other* event (the compositor eventually dropping the connector to DPMS off) turns it off on a much longer schedule. A manual unlock→lock cycle drives the TV correctly, so the lock/unlock *transition* path is healthy.

> Note: the original field report inferred that TisVlay "treats the session as unlocked because there was no lock edge, so the locked-idle path is never armed." **Inspecting the archived boot journald refutes that** — the locked-idle watch *was* armed at boot. A live `swayidle` probe then **ruled out the compositor** (KWin delivers idle while locked), and the defect is an **`is_locked` startup seed race** inside tisvlay-session. See [Root cause](#root-cause-localized-to-tisvlay-session) below.

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

## Root cause (localized to tisvlay-session)

The failure is **inside tisvlay-session**, between a delivered idle transition and a dispatched action — not in the compositor and not in watch registration.

Two hypotheses were open after reading the archived logs:

- **(A) KWin does not deliver `ext-idle-notify` idle while the session is locked.**
- **(B) TisVlay receives the transition but drops it before dispatch** (a seat-local gate or a seed race).

A live probe settled it (see [Probe: KWin delivers idle while locked](#probe-kwin-delivers-idle-while-locked)): a plain `ext-idle-notify` client got its `Idled` transition ~5 s into the locked period, so **(A) is refuted**. Since tisvlay-session had the identical 5 s watch armed at boot (evidence #2), the compositor was delivering transitions the session failed to act on. **(B) is confirmed** — specifically an **`is_locked` startup seed race**.

The two initial atomics are asymmetric ([`serve`](../../crates/tisvlay-session/src/lib.rs)): `is_active` defaults to **`true`** and `is_locked` defaults to **`false`**. The active-session gate is therefore *not* at fault — `is_active` starts true, and logind reports `Active=yes` even while locked (verified with `loginctl show-session`), so a seat-local event is never suppressed for inactivity here. Only `is_locked` starts wrong-for-a-locked-boot and depends on the lock watcher seeding it from `LockedHint`.

The idle adapter arms two tags that gate on **opposite** lock states ([`idle_dispatch`](../../crates/tisvlay-session/src/lib.rs)):

```
(LOCKED_IDLE_TAG,   Idled) if  is_locked  => IdleTimeout   // 5s
(UNLOCKED_IDLE_TAG, Idled) if !is_locked  => IdleTimeout   // 300s
```

`ext-idle-notify` fires `Idled` **once per idle period** (confirmed by the probe: the `IDLE locked=yes` line produced no repeat over 24 s). On an untouched boot lock screen there is no input, so each tag fires exactly once, and a seed landing between the two firings poisons both:

1. Boot into a locked session; `is_locked` starts `false`; the lock watcher has not seeded yet.
2. **t+5 s:** `LOCKED_IDLE` fires `Idled`. `is_locked == false` → guard fails → **dropped.**
3. The lock watcher seeds `is_locked = true` (reads `LockedHint=yes`), sometime after t+5 s.
4. **t+300 s:** `UNLOCKED_IDLE` fires `Idled`. `is_locked == true` now → `if !is_locked` fails → **dropped.**

A single seed landing anywhere in `(5 s, 300 s)` drops **both** transitions — the exact "no idle action at all, ever" symptom. This corrects an earlier note here that argued the 300 s path should have fired: it does not, because the seed flips the flag *between* the two once-only firings. It also matches why a **warm** dev-stack start cannot reproduce it (the seed lands in <1 s when the system bus is already up) — the bug lives only in the cold-boot window where the `LockedHint` seed lands after the 5 s idle.

The original "isolation test" (`loginctl unlock-session && lock-session`) only exercised lock/unlock *transitions*; it never sat idle while locked, so it never hit the race. Evidence #6 (INFO-only logging) is why the archived logs cannot show the dropped transition directly: the `debug!("session event")` dispatch line and the `trace!("Wayland idle transition")` seam were never written.

## Probe: KWin delivers idle while locked

Run on the same machine (a live KDE Plasma 6 Wayland session), using `swayidle` as an independent `ext-idle-notify` client that tags each transition with the live `LockedHint`:

```
nix run nixpkgs#swayidle -- -d -w \
  timeout 5 'echo "$(date +%T) IDLE   locked=$(loginctl show-session $SID -p LockedHint --value)"' \
  resume  'echo "$(date +%T) RESUME locked=$(loginctl show-session $SID -p LockedHint --value)"'
```

Wiggle the mouse, lock the screen, sit idle ~15 s, unlock. Result:

```
21:22:07 IDLE   locked=no      # control: unlocked idle works
21:22:13 RESUME locked=no
21:23:52 IDLE   locked=yes      # <-- KWin delivered idle WHILE LOCKED
21:24:16 RESUME locked=yes
21:24:28 IDLE   locked=no       # after unlock
```

The `IDLE locked=yes` line is the whole finding: the client received the `Idled` transition while locked, so the wayland transport is not the gap. (The eventual DPMS-off in the field report is consistent with this — PowerDevil's "turn off screen" uses KWin's internal DPMS path, a separate schedule from the client-facing idle stream.)

## Risk

Any boot-into-locked configuration (a very common KDE/greeter setup) leaves an OLED panel lit on the login/lock screen for the full DPMS timeout instead of the intended few seconds — the exact burn-in exposure the idle protection exists to prevent.

## Mitigation options

The compositor is ruled out and the cause is a startup ordering race, so the fix is a session-side seeding change. Do **not** implement the original "synthetic lock event at startup" — the watch is already armed; that changes nothing.

- **Recommended — synchronous seed before the idle adapter starts.** Do a one-shot logind `LockedHint` read up front and store it into `is_locked` *before* `idle_adapter` is spawned, then let the supervised lock watcher continue for live edges. No idle transition can then dispatch against an un-seeded flag. This is structurally the same startup-robustness pattern the greeter uses (assert initial state before watching), applied as a seed rather than a synthetic event.
- **Alternative — lazy re-read on first idle.** When a `LOCKED_IDLE`/`UNLOCKED_IDLE` `Idled` arrives and the seed may not have landed, re-read `LockedHint` at that instant instead of trusting the cached atomic. Couples the idle adapter to logind, so less clean than the synchronous seed.
- Whichever is chosen, keep the supervised watcher for reconnect/live edges — the fix is only about the *initial* value being correct before the first dispatch.

## Workarounds (config-side, no code change)

- Set `Autolock=true` (with a short idle) in `kscreenlockerrc` so a genuine idle→lock **transition** occurs after login, or
- Emit `loginctl lock-session` from a user-session oneshot shortly after login to synthesize the edge.

## Reproducers

1. **Compositor discriminator (done).** The `swayidle` probe above — ruled out the compositor, no reboot, no tisvlay changes. Kept for regression: if a future desktop *does* withhold idle while locked, this probe re-detects it.
2. **Automated regression test (the fix's guard).** In [`crates/tisvlay-session/tests/`](../../crates/tisvlay-session/tests/): boot the session stack on a temp socket with fake logind reporting `LockedHint=yes` **before** the session starts (so the seed must be observed at startup, not via an edge), hydrate a TV with `locked_idle_timeout_seconds`, sit idle past the locked timeout, and assert a screen-off rpc fires. Pre-fix this fails (the locked-idle transition is dropped against `is_locked == false`); post-fix it passes because the synchronous seed set `is_locked` before the idle could dispatch. Note a *warm* start does not reproduce the field race — the test must assert the seed is in place before the first idle, not rely on timing.
3. **DEBUG boot (optional field confirmation).** Boot into the locked session with the session unit at `RUST_LOG=tisvlay_session=debug,tisvlay_core=trace`, sit idle past 5 s: `Wayland idle transition` present but no `session event` confirms the `is_locked` guard dropped it in the field.

## Re-evaluation triggers

- The fix (synchronous seed) + automated regression test land (closes the concern).
- Any change to the KWin / `ext-idle-notify` idle-while-locked behavior lands upstream (re-run the probe).
- The idle adapter or lock watcher is refactored for another reason.

## Possibly related / separate

The **managed CEC device** on `/dev/cec0` fails its power-on handshake (`Backend error (cec): TV did not report power-on after repeated Image View On`), while the LG network backend works. Likely a distinct issue, noted here only for context.
