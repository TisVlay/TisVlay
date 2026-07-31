# Session that boots already-locked never triggers the idle screen-off

**Priority**: medium
**Found by**: field report on the C3 (LG OLED42C3PUA), NixOS + KDE Plasma 6 Wayland
**Status**: compositor ruled out by a live `swayidle` probe; root cause localized to tisvlay-session (a seat-local dispatch gate), pending a DEBUG boot to pinpoint the exact gate

## Summary

On a desktop that boots straight into a locked session (KDE `LockOnStart=true`, greeter-to-locked handoff, autologin-then-lock), idling on the boot lock screen never fires the configured **locked** idle timeout. The TV stays on until some *other* event (the compositor eventually dropping the connector to DPMS off) turns it off on a much longer schedule. A manual unlock→lock cycle drives the TV correctly, so the lock/unlock *transition* path is healthy.

> Note: the original field report inferred that TisVlay "treats the session as unlocked because there was no lock edge, so the locked-idle path is never armed." **Inspecting the archived boot journald refutes that** — the locked-idle watch *was* armed at boot. A live `swayidle` probe then **ruled out the compositor** (KWin delivers idle while locked), localizing the defect to a seat-local dispatch gate inside tisvlay-session. See [Root cause](#root-cause-localized-to-tisvlay-session) below.

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

A live probe settled it (see [Probe: KWin delivers idle while locked](#probe-kwin-delivers-idle-while-locked)): a plain `ext-idle-notify` client got its `Idled` transition ~5 s into the locked period, so **(A) is refuted**. Since tisvlay-session had the identical 5 s watch armed at boot (evidence #2), the compositor was delivering transitions it failed to act on. **(B) is confirmed as the family**; the specific gate remains to be pinned:

- **The active-session gate (prime suspect).** [`dispatch`](../../crates/tisvlay-session/src/lib.rs) suppresses *every* seat-local event — `IdleTimeout` included — when `is_active` is false: `if event.is_seat_local() && !self.is_active.load() { return }`. If `is_active` was not seeded true at boot (the lock watcher seeds it after acquiring the session), *both* the locked and unlocked idle paths are silently suppressed. This is the only single cause that explains **no idle action at all, ever** — the observed symptom.
- **The `is_locked` seed.** `(LOCKED_IDLE_TAG, Idled) if is_locked` drops the locked transition if the flag was not seeded true. But this *alone* is insufficient: with `is_locked == false`, the 300 s unlocked watch would have fired `IdleTimeout` at ~10:54 — which never happened. So a bad `is_locked` seed can only be a contributing factor, not the whole story.

The original "isolation test" (`loginctl unlock-session && lock-session`) only exercised lock/unlock *transitions*; it never tested idle-*while*-locked, so it never touched the gate at fault. Evidence #6 (INFO-only logging) is why the archived logs cannot name the exact gate: the `debug!("session event")` dispatch line — which logs `automation suppressed (session not active)` — and the `trace!("Wayland idle transition")` seam were never written.

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

The compositor is ruled out, so the fix is session-side. Do **not** implement the original "synthetic lock event at startup" — the watch is already armed; that changes nothing.

- **Active-session gate (prime suspect):** ensure `is_active` is seeded true before the first seat-local event can dispatch, or re-read `Active` when a seat-local event arrives with `is_active == false` rather than trusting a possibly-unseeded flag. A boot-locked session *is* the seat's foreground (`Active=yes`, `LockedHint=yes`), so the gate should not suppress it.
- **`is_locked` seed:** harden the ordering so the seed is observed before the first idle can dispatch, or re-check `LockedHint` on the first `Idled` rather than trusting the cached flag.
- Both share a root shape — a seat-local idle transition can be delivered before the logind seeds land. The durable fix is to make the first delivered transition tolerant of an un-seeded gate (lazy re-read) rather than racing the seeds into place.

## Workarounds (config-side, no code change)

- Set `Autolock=true` (with a short idle) in `kscreenlockerrc` so a genuine idle→lock **transition** occurs after login, or
- Emit `loginctl lock-session` from a user-session oneshot shortly after login to synthesize the edge.

## Reproducers

Two, at different tiers:

1. **Compositor discriminator (done).** The `swayidle` probe above — settled A vs B, no reboot, no tisvlay changes. Kept for regression: if a future desktop *does* withhold idle while locked, this probe re-detects it.
2. **DEBUG boot (pinpoints the gate).** Boot into the locked session with the session unit at `RUST_LOG=tisvlay_session=debug,tisvlay_core=trace`, sit idle past 5 s, then grep the journal:
   - `automation suppressed (session not active)` present → the active-session gate (`is_active` unseeded) is the cause.
   - `session event` with `IdleTimeout` present but no TV rpc → look downstream (automation gate / rpc).
   - `Wayland idle transition` present but no `session event` → the `is_locked` guard dropped it.
3. **Automated regression test (the fix's guard).** In [`crates/tisvlay-session/tests/`](../../crates/tisvlay-session/tests/): boot the session stack on a temp socket with fake logind reporting `LockedHint=yes` **and** `Active=yes` before the session starts, hydrate a TV with `locked_idle_timeout_seconds`, inject an `Idled` on the locked tag through the test driver, and assert a screen-off rpc fires. A second case with `Active=no` asserts the gate suppresses — locking down both directions.

## Re-evaluation triggers

- The DEBUG boot is captured (names the exact gate).
- The automated regression test lands (closes the concern).
- Any change to the KWin / `ext-idle-notify` idle-while-locked behavior lands upstream (re-run the probe).
- The idle adapter or lock watcher is refactored for another reason.

## Possibly related / separate

The **managed CEC device** on `/dev/cec0` fails its power-on handshake (`Backend error (cec): TV did not report power-on after repeated Image View On`), while the LG network backend works. Likely a distinct issue, noted here only for context.
