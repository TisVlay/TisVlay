# Report: Moonshine streamed-gamepad input spuriously wakes the LG TV via TisVlay

Handoff for a TisVlay-repo agent. Written from the *consumer* side (a NixOS host running both Moonshine and TisVlay), so treat the TisVlay code citations as leads to verify against `HEAD`, not gospel. The central question this report exists to answer is **which project should own the fix — TisVlay or Moonshine — given that suppressing wakes wholesale would break a legitimate local user.**

## TL;DR

Moonshine (a headless game-streaming server) creates its virtual gamepad as a **real, non-isolated `seat0` `uhid` device**. When a remote Moonlight client drives that gamepad, the input events land on the host's `seat0` — the same seat as the physical desk. Two host-side consumers then react to that leaked input: (1) the desktop compositor (Plasma/KWin) resets its idle/lock state, and (2) TisVlay's `ext-idle-notify` watcher sees the resulting `Resumed` edge and fires a wake action that powers the LG TV on. The user sees the physical TV/desktop un-blank every time they nudge the controller in a stream.

The naive fix (arm TisVlay's `while_remote` gate for the stream's lifetime, which *does* suppress the wakes) is **wrong as a complete solution**: it suppresses *all* wakes while a remote session is active, so a person physically at the desk could no longer wake the TV while a remote Moonshine session happens to be connected. The real requirement is to distinguish **leaked remote input** from **genuine local input** — and that distinction, plus the fact that Plasma reacts to the leak too, is what makes this arguably a Moonshine-side (input-isolation) bug rather than a TisVlay-side one.

## Environment / roles

- Host `pox`: NixOS, Plasma 6 **Wayland**, single user (`uid 1000`) autologged into an **active `seat0`** session on `tty1`. The **LG TV is `pox`'s physical display** (`card1-HDMI-A-1`), and TisVlay manages its power (WebOS + a DPMS/DRM watcher).
- Moonshine 0.15.0 runs as a headless system service (`User=<user>`, lingering). Each stream spins up its own **headless Wayland compositor** (a virtual output at the client's requested mode, e.g. `3120x1440@120`) and launches Steam Big Picture; the game runs in a transient `moonshine-session.service` under the user manager. Moonshine holds only `/dev/dri/renderD128` (render node) — it never touches `card1` (KMS scanout, held by the desktop's Xwayland).
- Moonlight client: Android, with a **DS4** → Moonlight reports it as a PlayStation pad → Moonshine emulates a DualSense (`"Moonshine PS5 controller"`, `054c:0ce6`) via `uhid`.

## Symptom & the user's own observation

Streaming works; controller input works in-game (after enabling Steam's PlayStation controller support). But: **interacting with the controller wakes the physical LG TV.** Reproduced by the user directly: *"I waited for the lockscreen idle to re-blank the screen, then touched the controller again to navigate Steam Big Picture, and I saw the screen un-blank."* The desk was unattended the whole time — nobody physically touched a local input device.

## Evidence (host logs)

- TisVlay daemon logs `LG TV online after wake` repeatedly, tightly clustered with Moonshine sessions and gamepad activity. Over one ~4-minute session cluster: wakes at `11:26:52, 11:28:00, 11:29:10, 11:30:21`, each within seconds of a compositor start / `Gamepad 0 connected`. During each session it also logs `Stale connection, reconnecting` → `backend command failed` (the stream's traffic disrupts its WebSocket to the TV; secondary, not the wake cause).
- The wake at `11:28:00` fired **6s after the stream started (`11:27:54`) and 6s *before* the gamepad device was even (re)created (`11:28:06`)** on that particular cycle, i.e. session-up already produces edges; and subsequent nudges reproduce it. A fresh `uhid` device is created per session (`…0016`, `…0017`, … `…001B`).
- **No system suspend/resume** (`PrepareForSleep`) and **no DRM/DPMS connector uevents** anywhere in the window. So it is *not* a real sleep/resume and *not* a physical-connector DPMS edge from Moonshine.
- The kernel logs the gamepad as a normal `seat0` HID device: `playstation 0005:054C:0CE6.001A: hidraw15: BLUETOOTH HID … [Moonshine PS5 controller]` + `input: Moonshine PS5 controller as /devices/virtual/misc/uhid/…/input/input62`. It carries a `uaccess` ACL for the active-seat user (confirmed via `getfacl` on the pad's `hidraw`/`event` nodes in an earlier session) — which is exactly why it is treated as a `seat0` device.
- `loginctl show-session 2` (the desktop): `Type=wayland, Active=yes`. `LockedHint` toggles to `yes` when the desk idle-locks during a stream.

## Root-cause mechanism

### Moonshine side (the source of the leak)

Moonshine emulates the controller **type the client reports** — `moonshine-core/src/session/stream/control/input/gamepad.rs` (`enum GamepadKind { Xbox, PlayStation, Nintendo, Unknown }`; `PlayStation → PS5Joypad` = the `uhid` DualSense). Keyboard/mouse are *injected into Moonshine's isolated compositor*, but the **gamepad is created as a real host `uinput`/`uhid` device via inputtino**, so it is a first-class `seat0` input device, not isolated to the stream.

Moonshine already knows this is a limitation:
- README: *"this does not allow multi-seat gaming using controllers, as these are not isolated."*
- **Open issue hgaiser/moonshine#38 — "[Feature request] Isolate gamepad/joypad/gaming controllers"** — the exact request to isolate controllers so they don't bleed onto the host seat.
- Issue #67 (maintainer): confirms controllers rely on the running user controlling the active seat (uaccess), i.e. they are seat devices by design.

### Host compositor + TisVlay (the two consumers)

Because the pad is on `seat0`, the **desktop Plasma/KWin** treats its activity as input and resets its own idle/lock (this is why the physical screen un-blanks even before considering TisVlay). TisVlay then reacts to the same edge:

- TisVlay's idle adapter binds `ext-idle-notify-v1` on **whatever `$WAYLAND_DISPLAY` the session process has** — `crates/tisvlay-core/src/wayland.rs` `start_idle_watch` → `Connection::connect_to_env()` (~:182), `ext_idle_notifier_v1` on `wl_seat` (~:291–305). **No seat/output scoping** — it watches one compositor by env, and forwards every `Idled`/`Resumed`. On this host it attached once at boot to the *desktop* compositor and never re-attached (only one `Wayland idle watch started` in `journalctl --user -u tisvlay-session`).
- A **200 ms "lock-wake" probe is registered unconditionally**, independent of any configured idle timeout — `crates/tisvlay-session/src/lib.rs` (~:1352–1355; `LOCK_WAKE_TIMEOUT_MS` ~:1308). The two real idle watches are added only if their timeout is non-zero, and both default to 0/disabled (`config.rs` ~:624). So out of the box the 200 ms probe is the live listener.
- Edge → event mapping: `idle_dispatch` / `lock_wake_policy` (`lib.rs` ~:1436–1499). A `Resumed` fires `LockWake` iff the mirrored `is_locked == true`; else, if the unlocked-idle watch is enabled, `IdleResume`. Lock/Active state is mirrored from the **physical seat's** `LockedHint` (`crates/tisvlay-session/src/lock.rs`, seat-scoped: `scan_sessions_for_own_seat` requires `uid == own_uid && !seat.is_empty()`), so during a stream `is_locked` reflects the real desk locker.
- Wake events map to `ScreenOn`/`power-on` at the config floor — `crates/tisvlay-core/src/config.rs` (~:609–645): `on_lock_wake` (~:623), `on_idle_resume` (~:627), `on_local_return` (~:642) — dispatched via `Core::dispatch` → `call_tv` (`lib.rs` ~:810–895) → the daemon's `set-screen {on:true}` / `power-on`.
- **Why it isn't already suppressed:** the `while_remote` gate (`config.rs` ~:102–182, applied `lib.rs` ~:922–928) only arms when a remote-session detector fires, and **every detector hard-codes Sunshine**: `sunshine.rs` (`SERVERINFO_ADDR = 127.0.0.1:47989`, `is_sunshine_comm` requires comm prefix `"sunshine"`), `monitor.rs` (portal allow-list `["krdpserver","rustdesk","sunshine"]`), `logind.rs` (needs a `Remote=yes` graphical logind session). Moonshine matches none. This exact gap is already noted in `docs/ideas/remote-session-followups.md` ("Steam Remote Play / Parsec … portal-invisible on Nvidia like Sunshine … each would need its own signal probe"). Moonshine is in that bucket.

The daemon's own reactors are **not** the trigger and can be ruled out: `logind.rs` (~:44–62) only edge-triggers on real `PrepareForSleep(false)`; `dpms.rs` (~:25–52) only polls a **physical connected connector's** sysfs `dpms` (a virtual output never touches it); `seat.rs` (~:203) only reconciles a socket ACL on seat changes.

## The design tension (why the obvious fix is wrong)

Arming `while_remote` for the stream's lifetime suppresses `on_lock_wake` / `on_idle_resume` / `on_local_return` — verified working: `tisvlay remote-session start --source moonshine` armed both TVs (`sources=[{"detail":"moonshine","kind":"manual"}]`); `stop` cleared it. **But that is a blunt instrument:** it suppresses *all* wakes for the duration, so it conflates two very different actors:

- **Leaked remote input** (the Moonshine `uhid` pad driven by a remote client) — should NOT wake the local TV.
- **Genuine local input** (a person at the desk pressing a key, moving the mouse, or picking up a *local* controller) — SHOULD wake the local TV, even while a remote Moonshine session is connected.

A correct fix must preserve local wake during an active remote session. `while_remote` alone cannot: it keys on "is any remote session active", not "did this particular resume come from a local device". So the manual arm is only acceptable as a temporary stopgap, not the design.

## The pivotal open question (please run this experiment first)

Is TisVlay the *only* thing that reacts to the leaked pad, or does the desktop compositor react independently? This decides whether a TisVlay-only fix can even be complete.

**Experiment:** start a Moonshine stream, let the desk idle-lock and blank, `systemctl --user stop tisvlay-session` (and/or the daemon), then nudge the controller.
- If the physical screen **still un-blanks** with TisVlay stopped → the leak is host-wide (Plasma/KWin resets idle/DPMS on the seat0 pad). A TisVlay-only fix cannot fully solve it; the correct place is **Moonshine (isolate the pad)**.
- If the screen **stays blank** with TisVlay stopped → TisVlay is the sole waker, and a TisVlay-side fix is sufficient.

Also confirm which compositor TisVlay's idle watch is bound to and that gamepad-only input produces the `Resumed` (raise `tisvlay-session` to debug; look for `Wayland idle watch started` around a stream and the `LockWake`/`IdleResume` → `ScreenOn` dispatch lines). On this host the watch was on the desktop compositor, implying Plasma itself counts the pad — but verify.

## Candidate fixes and trade-offs

### A. Moonshine-side: isolate the streamed gamepad from the host seat (issue #38) — *likely the correct fix*
If the streamed pad is not a `seat0` device (own seat, or otherwise invisible to the desktop compositor's input/idle), the leak disappears at the source: neither Plasma nor TisVlay reacts, and **local input still wakes normally with zero TisVlay change**. Downsides: it's an open, non-trivial Moonshine feature (multi-seat/inputtino isolation is hard), and TisVlay would remain vulnerable to *other* un-isolatable streamers (Steam Remote Play, Parsec). Action for TisVlay repo: probably none beyond documenting the interaction and supporting/nudging moonshine#38.

### B. TisVlay-side: source-aware wake (not blunt `while_remote`) — *the right shape if TisVlay must own it*
Make the wake decision depend on the **device that caused the resume**, not merely "is a remote session active." Because `ext-idle-notify-v1` does **not** report the source device, TisVlay would need to add a lightweight input-activity watcher (evdev/libinput) and classify devices, then treat a `Resumed` as a wake only if it coincides with activity from a **local** device. Classification heuristics for "not local": device name prefix `"Moonshine "` (and Sunshine's `"Sunshine ..."`), `uhid`/virtual devices under `/devices/virtual/…`, or devices tagged by the streamer. This preserves local wake during remote sessions. Downsides: new subsystem, per-streamer naming knowledge, and it still can't stop *Plasma* from reacting (see the experiment — if Plasma un-blanks on its own, this only fixes TisVlay's half).

### C. TisVlay-side: auto-detect Moonshine, then apply B's gating (not A's blunt suppression)
Add a Moonshine remote-session detector so the "remote" fact is known automatically — poll Moonshine's serverinfo on **47989** (GameStream/Moonlight; note it reuses Sunshine's port but a different server string), or match the `moonshine` process, or the `_nvstream._tcp` mDNS advert. But use it to *inform* the source-aware gating of B, not to blanket-suppress. On its own (blanket suppress) it inherits the local-wake regression.

### D. Stopgap available today: the manual hook
Moonshine has per-application `pre_command`/`post_command`. Bracketing Big Picture with `tisvlay remote-session start/stop --source moonshine` arms the gate for the session (validated). Ships relief now, but carries the local-wake regression of §"design tension" and only covers apps launched through that entry. Documented manual verb: `docs/remote/detection.md` (~:13,19).

## Recommendation

Lead with the experiment. If Plasma un-blanks on its own (expected on this host), the **complete** fix is **A (Moonshine isolation, #38)**; TisVlay's role is defense-in-depth for un-isolatable streamers, and that role should be **B** (source-aware wake), never blanket `while_remote` suppression. If Plasma does *not* react and TisVlay is the sole waker, **B** (optionally fed by **C**) fully solves it while preserving local wake. **D** is the interim.

## Repro / diagnostics summary

- Wake correlation: `journalctl -u tisvlay-daemon` → `LG TV online after wake` clustered with `journalctl -u moonshine … 'Gamepad 0 connected'`.
- Idle-watch binding + events: `journalctl --user -u tisvlay-session` (raise to debug); `Wayland idle watch started`, `LockWake`/`IdleResume`, `ScreenOn`.
- Seat/lock state: `loginctl show-session <desktop-session> -p LockedHint -p Active`.
- The pivotal experiment: stream + idle-blank + `stop tisvlay-session` + nudge controller → does the physical screen still un-blank?
- Manual gate check: `tisvlay remote-session status` before/during/after `start|stop --source moonshine`.

## Key file references

- TisVlay: `crates/tisvlay-core/src/wayland.rs` (env-bound `ext-idle-notify`, no seat scoping), `crates/tisvlay-session/src/lib.rs` (unconditional 200 ms lock-wake probe; `idle_dispatch`/`lock_wake_policy`; `dispatch`/`call_tv`; `is_active` gate), `crates/tisvlay-session/src/lock.rs` (seat-scoped `LockedHint`/`Active`), `crates/tisvlay-core/src/config.rs` (wake-action floor + `while_remote`), `crates/tisvlay-session/src/{sunshine,monitor,logind}.rs` (Sunshine-only detectors), `crates/tisvlay-daemon/src/{logind,dpms,seat}.rs` (daemon reactors, ruled out), `docs/ideas/remote-session-followups.md`, `docs/remote/detection.md`.
- Moonshine: `moonshine-core/src/session/stream/control/input/gamepad.rs` (`GamepadKind`, `uhid` PS5 pad), README multi-seat/controller-isolation caveat, issues #38 (isolate controllers) and #67 (active-seat requirement).
