## The daemon takes its sleep and shutdown inhibitors separately (a logind workaround)

**Priority**: low
**Found by**: a KDE neon (Ubuntu 24.04 base) field check, where the daemon logged `could not take logind inhibitor (continuing without)` at every start

### Context

The daemon holds a logind *delay* inhibitor so the system profile's sleep and shutdown actions reach the TVs before the machine goes down (`crates/tisvlay-daemon/src/logind.rs`). It used to take one lock, `what = "sleep:shutdown"`. That is the documented API: `what` is a colon-separated list, and logind should check each listed kind's polkit action.

logind in systemd 255 didn't. `method_inhibit` picked a single polkit action by comparing the whole `what` against each single kind (`w == INHIBIT_SHUTDOWN ? … : w == INHIBIT_SLEEP ? … :`), so a combined request matched none of them and fell through to `org.freedesktop.login1.inhibit-handle-lid-switch`. That action's implicit `any` is `no`, which refuses a caller without a session, like a system daemon. On the neon VM, `systemd-inhibit --what=sleep:shutdown --mode=delay` failed with "Access denied" for every non-root account, while `sleep` alone and `shutdown` alone both succeeded. Without the lock, logind doesn't wait, and the TVs' power-off races the suspend.

Upstream bug: [systemd#33834](https://github.com/systemd/systemd/issues/33834), "Wrong polkit policy checked when adding an inhibitor lock that blocks multiple actions". Fixed by [639719e0](https://github.com/systemd/systemd/commit/639719e01065c3a2f557d70e4d8088c2ec71c7c6) (PR #33838, merged 2024-07-25), which checks each requested kind's action in turn. It shipped in v257 and was backported to the v255 stable series from v255.11 and to v256 stable (present in v256.11). Ubuntu 24.04 ships `255.4-1ubuntu8.18`, Ubuntu's own revision of upstream 255.4 without the backport, so every 24.04-based distro (KDE neon, Linux Mint 22, Pop!_OS 24.04) has the bug. A systemd 262 bench takes the combined lock fine. (Checked 2026-11-06 against the systemd v255, v256, v257, v258 and v262 sources and the systemd-stable v255.x tags.)

The daemon now takes one delay lock per kind, two `Inhibit` calls, and releases both together. On a fixed logind that behaves exactly like the combined lock. The test fake logind (`crates/tisvlay-test/src/fake_logind.rs`) refuses a combined `what` the way the unpatched logind does, so the daemon's inhibitor tests catch a return to the combined form.

### Risk

Low. The workaround is one code path with no version check, and costs one extra inhibitor entry in `systemd-inhibit --list`. The debt is only that it, and the fake's refusal, exist for a bug fixed upstream.

### Mitigation options

- **Keep the per-kind locks** (preferred while supported distros ship an unpatched logind).
- **Revert to the combined lock** once none do: revert the commit that introduced this doc, which takes the daemon change, the fake's refusal, the per-kind test and this doc out together.

### Re-evaluation triggers

- The 24.04-based distros move to a newer base (Ubuntu 26.04 ships a fixed systemd), or TisVlay drops 24.04-based support.
- Ubuntu backports the fix into 24.04's systemd package.
