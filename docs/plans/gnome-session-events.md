# GNOME session events: idle and lock over GNOME's own transports

Status: **in progress** on branch `fix/gnome-patch`. Diagnosed and empirically validated on an Ubuntu 26.04 / GNOME Shell 50.1 Wayland VM (2026-07-19); implementation landing as reviewable diffs off this baseline.

## Problem

Five profile-driven session events fire on KDE Plasma but not on GNOME: `IdleTimeout`, `IdleResume`, `LockWake` (all idle-derived) and `Lock` / `Unlock`. The session service's two transports are both KDE-shaped and silently no-op on GNOME:

- **Idle** rides the `ext-idle-notify-v1` Wayland protocol. Mutter does not advertise `ext_idle_notifier_v1` — only KWin and wlroots implement it (confirmed on the VM: the registry lacks the global, and the adapter logs `ext_idle_notifier_v1 protocol not available` on a 5 s retry loop). This is an unimplemented Mutter feature, not a deprecation or a version-age issue — GNOME 50.1 is current for 2026.
- **Lock** watches `org.freedesktop.ScreenSaver` `ActiveChanged` / `GetActive`. On GNOME that bus name is owned by `gsd-screensaver`, but it implements **only** the standardized [Idle Inhibition spec](https://specifications.freedesktop.org/idle-inhibit-spec/latest/) (`Inhibit`/`UnInhibit`): `GetActive` returns an error ("not part of the idle inhibition specification") and `ActiveChanged` never fires. The lock-state methods (`GetActive`/`ActiveChanged`/`SetActive`/`Lock`) were never a freedesktop *specification* — they are a de-facto interface inherited from the long-removed `gnome-screensaver` and `xscreensaver`. KDE keeps implementing them on `org.freedesktop.ScreenSaver` for app compatibility ([kscreenlocker ships its own `org.freedesktop.ScreenSaver.xml`](https://github.com/KDE/kscreenlocker/blob/master/dbus/org.freedesktop.ScreenSaver.xml)); GNOME implements only the spec and relocated lock-state to `org.gnome.ScreenSaver`. So the working KDE path leans on a KDE-specific extension, not a cross-desktop standard.

The remote-session events (`RemoteConnect`/`RemoteDisconnect`/`LocalReturn`) ride the portal / PipeWire-screencast / logind / Sunshine detectors and are GNOME-native or already fixed (the screencast SPA-plugin-dir panic was resolved in the Debian-packaging pass); they are out of scope here.

## Design

Two independent transports, each selected by what the running desktop actually provides. The shared vocabulary (`IdleNotification { tag, timeout_ms }`, `IdleTransition::{Idled, Resumed}`, and the `SessionEvent` set) is unchanged — only the producers are new, so the idle dispatcher and every profile action stay transport-agnostic.

### Idle → `org.gnome.Mutter.IdleMonitor` (added alongside wayland)

Mutter's D-Bus idle service is present on every GNOME session (`AddIdleWatch`/`AddUserActiveWatch`/`GetIdletime`/`RemoveWatch` + `WatchFired`). Its model, per libgnome-desktop's `GnomeIdleMonitor`: an idle watch from `AddIdleWatch(interval)` fires `WatchFired` once the seat has been idle for `interval` and **re-arms** for the next cycle; a watch from `AddUserActiveWatch()` fires once when the user next becomes active and is then **auto-removed** (one-shot). We map that onto the same `(tag, IdleTransition)` stream `ext-idle-notify` emits:

- One persistent idle watch per notification (`AddIdleWatch(timeout_ms)`), its returned id mapped to the caller's tag. `WatchFired(idle_id)` → `(tag, Idled)`.
- A single one-shot active watch, armed the first time any watch idles within a cycle, detects the resume. `WatchFired(active_id)` → `Resumed` for **every** tag that idled this cycle (Mutter's active watch is global, not per-interval), then re-armed on the next idle. This reproduces `ext-idle-notify`'s per-notification `Idled`/`Resumed` pairing closely enough that the dispatcher — including the 200 ms `LockWake` probe and its per-cycle latch — behaves identically.

The bookkeeping (idle-id→tag map, idled set, live active-watch id) is a pure `Tracker` folding `WatchFired(id)` into `Forward(tag, transition)` / `ArmActiveWatch` actions — unit-testable without GNOME, matching the screencast detector's pure-tracker + thin-zbus-strip split. A dedicated session-bus connection per watch means dropping the handle closes the bus and lets Mutter reap the watches; no explicit `RemoveWatch` teardown.

### Lock → logind `LockedHint` (replaces `org.freedesktop.ScreenSaver`)

The research reframed the choice: rather than chase two per-desktop de-facto ScreenSaver interfaces (`org.freedesktop.ScreenSaver` on KDE, `org.gnome.ScreenSaver` on GNOME), unify on the one genuinely standardized, systemd-native, cross-desktop lock-state signal — `org.freedesktop.login1.Session` `LockedHint`. Both desktops set it (kscreenlocker via [its logind MR](https://invent.kde.org/plasma/kscreenlocker/-/merge_requests/10); gnome-shell natively). The logind `Lock`/`Unlock` *signals* are deliberately **not** used: they are external lock *requests*, not state, and famously do not fire on a DE-initiated lock — `LockedHint` (the property) is the state the locker writes.

Mechanics mirror the existing logind remote detector on the system bus:

- Enumerate own-uid **graphical** sessions (`ListSessions` + `Type ∈ {x11, wayland}` + own uid), reusing the remote detector's `is_graphical` / `own_uid` filter. The session service itself is **not** in a logind session (it runs in the user@ manager scope — `GetSessionByPID` fails), so the graphical seat session must be found by enumeration, never by the service's own PID.
- Seed `is_locked` from the session's `LockedHint`, then follow `PropertiesChanged` for it (zbus's generated `receive_locked_hint_changed()` handles the subscription and caching). `true` → `Lock`, `false` → `Unlock`.
- Supervised connect loop with backoff, and a `SessionNew` re-hydrate for a graphical session that appears after the service starts — same shape as the remote detector, so a headless box or absent bus degrades quietly.

`screensaver.rs` and its session-bus lock wiring retire; the session bus stays only for the portal monitor.

## Implementation sketch

1. **`idle_mutter.rs`** (new): the `IdleMonitor` zbus proxy, the pure `Tracker`, and `start_idle_watch(notifications, tx) -> Handle` whose `Drop` aborts the task (closing the bus). Unit tests cover the tracker: first-idle arms the active watch, a second idle in the same cycle does not re-arm, the active edge resumes every idled tag together, a fresh cycle re-arms, and unknown ids are ignored.
2. **`idle_adapter`** (`lib.rs`): try `wayland::start_idle_watch` first; on `ProtocolNotSupported` or `ConnectionFailed`, fall back to `idle_mutter::start_idle_watch` on the session bus. A unified `IdleWatch` handle enum (`Wayland` | `Mutter`) stops the watch on drop; the retry/re-register loop and `idle_dispatch` are otherwise untouched.
3. **`lock.rs`** (new): the logind `LockedHint` watcher above; `pub use` it as `watch_lock` (or fold into a `spawn_lock_detector` on the system bus). Reuse `own_uid` / `is_graphical` from `logind.rs` (promote to `pub(crate)`); the lock-specific Session proxy adds the `locked_hint` property.
4. **`main.rs`**: drop the `watch_screensaver` spawn (and its session-bus branch, keeping the portal monitor); spawn the logind lock watcher on the system bus. `is_locked` is still the shared flag the idle lock-wake policy and the core's drift re-assert read.
5. **Tests**: extend `tisvlay-test`'s `fake_logind` to model a graphical session with a toggleable `LockedHint` (emitting `PropertiesChanged`) and return it from `ListSessions`; convert the two `screensaver_*` integration tests to drive lock/unlock through logind. Delete `fake_screensaver` once nothing references it.

## Verification

Diagnosis was confirmed live on the VM before writing code (2026-07-19):

- `busctl` — `org.freedesktop.ScreenSaver.GetActive` errors with the idle-inhibit-spec message; `org.gnome.ScreenSaver.GetActive` returns a bool; `org.gnome.Mutter.IdleMonitor` introspects with `AddIdleWatch`/`GetIdletime`/`WatchFired`.
- `gdbus monitor` on `org.gnome.ScreenSaver` — a real lock/unlock emitted `ActiveChanged (true)` then `(false)`.
- `dbus-monitor` on the system bus — the graphical session (`session/_32`, session 2, `Type=wayland`) emitted `PropertiesChanged {'LockedHint': <true>}` on lock and `<false>` on unlock; `LockedHint` also read `no`→`yes` across a lock via `loginctl`.

Exit check: install the built `.deb`, set low idle timeouts on the default profile, and confirm all five events fire on GNOME (journal + TV reaction). KDE re-verification is required because the lock path moved off `org.freedesktop.ScreenSaver` — `LockedHint` is set by kscreenlocker, so it is expected clean, but it is a behavior change on the working path.

## Non-goals

- No unified idle transport — there is no cross-desktop Wayland standard for arbitrary-timeout idle, so idle stays per-desktop (`ext-idle-notify` for KWin/wlroots, `Mutter.IdleMonitor` for GNOME). logind `IdleHint` is the DE's own single idle notion, not per-configured-timeout, and is unfit for the locked/unlocked idle dials.
- No change to the remote-session detectors or to the `SessionEvent` / profile-action vocabulary — only new producers behind the existing seam.
- No greeter idle on GNOME — the greeter remains `ext-idle-notify`-only (its targets are greetd/Plasma); GNOME/GDM greeter idle is separate work if it ever matters.
- No fallback chain on the lock side (logind primary + ScreenSaver secondary) — the unify-on-logind decision is deliberate; `LockedHint` covers both desktops, and a second path is code without a demonstrated gap.
