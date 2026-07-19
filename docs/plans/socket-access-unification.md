# Socket-access unification + upgrade-safe packaging

Move socket-access policy for the login greeter into the daemon (a greeter-group ACL applied at bind), and fix the maintainer scripts so upgrades don't tear down the running stack. Both came out of a field session on a Fedora 44 GNOME/GDM VM plus upstream research, and together they let us replace a set of per-distro grant hacks that were either broken or divergent.

## Motivation

The greeter socket-access mechanism we shipped (`usermod -aG tisvlay gdm` in the rpm/deb postinstall) is **wrong on the modern dynamic-userdb greeter** and only ever worked in theory on the retired static-`gdm` model. Testing on the VM also shook out two packaging-lifecycle bugs unrelated to the greeter. Rather than patch each distro's grant separately, the research points at one mechanism that is correct wherever the greeter's group is known — a daemon-applied group ACL — which also collapses the NixOS/packaged divergence.

## Findings (grounding)

Modern GDM (GNOME 49+: Fedora 43/44, Ubuntu 25.10+) allocates the greeter through systemd-userdb as a **dynamic** user `gdm-greeter`, uid in the 60578–60705 range, home under `/run/gdm/`, **primary group `gdm`** — not the static `gdm` daemon user (uid 42). A userdb/dynamic account draws no supplementary groups from `/etc/group`, so `usermod -aG tisvlay gdm` targets the wrong account and never reaches the greeter (**confirmed live on Fedora 44**: the greeter EACCES-exits). Upstream GDM solved the same problem for its own polkit/D-Bus policies in 2025 by keying on the **`gdm` group** (`subject.isInGroup("gdm")`), because that group is the stable handle across the migration.

A `g:gdm` **group ACL** on the socket (`g:gdm:rw`) plus `g:gdm:rx` on the runtime dir let the dynamic greeter connect — **validated live on Fedora 44**: the greeter got past its pre-flight, opened its zenoh session, and read the daemon's timeout key.

Two claims from this need reconciling before anything is deleted:

- **The Fedora "static greeter" claims in the repo are stale.** `packaging/scripts/postinstall.sh` and `docs/concerns/greeter-packaged-socket-access.md` still describe Fedora/RHEL/Arch as running the *static* `gdm` greeter (so `/etc/group` is honored). The VM proved Fedora 44 runs the *dynamic* greeter. Those comments/docs were written pre-validation and are now wrong; correcting them is part of this work, not a contradiction to route around.
- **The Ubuntu deferral is NOT closed yet.** `greeter-packaged-socket-access.md` records a *group* ACL (`g:gdm:rw`, mask rw) tested on Ubuntu and **failing**. We validated the ACL only on Fedora and never re-ran the Ubuntu test. The research says Ubuntu 25.10+ uses the same userdb greeter with primary group `gdm`, so the ACL is *expected* to work there too — but the recorded Ubuntu failure must be re-tested and explained on Ubuntu before we delete the Ubuntu deferral. Treat "Ubuntu solved" as a hypothesis to verify, not a result.

The greeter group's **name varies by distro** and must be resolved by name, never by gid: `gdm` on Fedora/RHEL/Arch/Ubuntu, **`Debian-gdm`** on Debian's gdm3 (gid 42 is Fedora-specific). Plasma-login is a separate greeter entirely — its greeter runs as `plasmalogin`, which is **not** in group `gdm`.

NixOS is a third case: `nixos/modules/services/display-managers/gdm.nix` pre-allocates the greeter as **static** accounts `gdm-greeter{,-2..5}` at uid 60578+ (static-emulating the dynamic greeter). Our current `users.groups.tisvlay.members` grant covers both those accounts *and* the plasma-login `plasmalogin` user; it works because they are static. Whether those static accounts carry group `gdm` (the precondition for the ACL to reach them) is **unconfirmed** and must be checked before the members grant is deleted.

Two lifecycle bugs (independent of the greeter):
- **Maintainer scripts don't distinguish upgrade from removal.** On an rpm upgrade the scriptlet order is new-`%post` → old `%preun`, so `preremove.sh` (which unconditionally stops + disables) runs *after* `postinstall.sh` enabled things and undoes them: the daemon is left stopped and units disabled. Reproduced via `dnf reinstall`.
- **No restart-on-upgrade** — even once the above is fixed, an upgrade needs an explicit restart or it keeps running the old binary.

Already landed this session as defensive context: the greeter pre-flights the socket and exits clean on `EACCES` (a stray-enabled greeter is inert, never loops), and the tray survives a missing StatusNotifier host instead of crash-looping (`assume_sni_available`). Both stand regardless of this plan.

## Workstream A — daemon-applied greeter-group ACL

At the existing post-bind step in `crates/tisvlay-daemon/src/lib.rs::open_socket` (creates the runtime dir, then `set_permissions(socket, 0o660)`), additionally apply a POSIX ACL granting a configured **greeter group**: `g:<grp>:rw` on the socket and `g:<grp>:rx` on the runtime directory (the greeter must traverse the `0750` dir *and* write the socket).

Load-bearing details the implementation must get right:
- **Config, wired everywhere — not just NixOS.** Take the group from a daemon option (proposed `--greeter-group <name>`, unset by default → no ACL). The value is the **active display manager's greeter group**: `gdm` (or `Debian-gdm`) for GDM, the plasma-login greeter's group for KWin. The NixOS module's `greeter.enable` passes it per DM, **and** the packaged `tisvlay-daemon.service` `ExecStart` must pass it too (currently it does not) — otherwise deleting the `usermod` grant leaves packaged greeters with no access at all. Since the packaged unit is one file across deb/rpm/arch, the default there is `gdm`/`Debian-gdm`; document that plasma-login packaging (if ever added) needs its own value.
- **Resolve by name, reentrantly.** Resolve `<grp>` via `getgrnam_r` (or an ACL/`nix`/`users` crate that uses it) — never the non-reentrant `getgrnam`, since the daemon runs concurrent NSS-touching adapters (netlink/arp/logind) that share libc's static buffer. Skip cleanly (no ACL, log once) when the group is absent.
- **Mask + chmod ordering.** Adding a named-group ACL entry creates a `mask` entry that also caps the owning `tisvlay` group's *effective* permission, and a later `chmod` recomputes/collapses that mask. So: apply the ACL **after** the `0660` chmod, set the mask to `rw` so both `tisvlay` and the greeter group keep effective `rw`, and never `chmod` the socket again after the ACL is set.
- **Reconcile, don't just add.** The unit sets `RuntimeDirectoryPreserve=restart`, so `/run/tisvlay` (and its ACL) survive daemon restarts; only the socket inode is fresh each bind. On every startup, **reconcile** the directory ACL to the current config — add the greeter-group entry when `--greeter-group` is set, **remove** it when unset — so clearing the flag and restarting actually revokes access instead of leaving a stale grant until a full stop.

This lets us **replace** (once the preconditions below hold): the rpm/deb `usermod -aG tisvlay gdm` + `gpasswd -d` lines; the NixOS `users.groups.tisvlay.members` greeter grant; and the concern doc's "secondary `tisvlay:gdm` socket" recommendation (a plain ACL on the main socket suffices on Fedora). Preconditions before each deletion:
- **Fedora/rpm:** the packaged daemon unit passes `--greeter-group gdm`; then the `usermod`/`gpasswd` lines go, and the stale "static gdm greeter" comment in `postinstall.sh` is corrected.
- **NixOS:** confirm the static `gdm-greeter` accounts carry group `gdm`; keep a grant for the plasma-login `plasmalogin` greeter (either `--greeter-group` set to its group, or retain the members grant for that account only). Do **not** delete the members grant blindly — validation was on a Fedora dynamic greeter, not the NixOS static accounts or plasma-login.
- **Ubuntu:** ~~re-test the ACL on Ubuntu before touching the Ubuntu deferral~~ — **done**: validated on Ubuntu 26.04 (gdm 50, dynamic greeter, `gdm` gid 975 resolved by name); the deferral is closed in the concern doc.
- **Upgrade hygiene:** hosts that installed a pre-ACL package added `gdm` to the `tisvlay` group; since removal-time `gpasswd -d` is going away, add a one-time `gpasswd -d gdm tisvlay` in `postinstall` so the obsolete standing membership is revoked when moving to the ACL model.

Security: the ACL widens socket reach from the `tisvlay` group to also the greeter group (`gdm`), a root-run, trusted display-manager group. It stays opt-in (unset by default) so a daemon with no greeter deployed grants nothing extra; the reconcile step above is what makes "opt-in" actually revocable.

## Workstream B — upgrade-safe maintainer scripts

Guard the removal-only logic in `preremove.sh` so it runs **only on real removal**, in a form correct for the single script nfpm shares across deb/rpm/arch — noting the three managers use three different `$1` conventions:
- rpm `%preun`: numeric count, `0` = final removal, `≥1` = upgrade/reinstall.
- deb `prerm`: string, `remove`/`purge` = removal, `upgrade`/`failed-upgrade` = upgrade.
- pacman `pre_remove`: the package **version string** (e.g. `0.3.0-1`), and pacman calls `pre_remove` **only** on removal (upgrades use `pre_upgrade`).

A guard that matches removal on all three by *excluding upgrades* rather than listing removals: `case "$1" in 1|upgrade|failed-upgrade) : skip ;; *) <stop/disable> ;; esac`. This skips on rpm `1`, deb `upgrade`, and never wrongly skips on pacman (version-string `$1` falls to the default → cleanup runs, which is correct since pacman only reaches `pre_remove` on removal). A naive `[ "$1" = 0 ] || [ "$1" = remove ]` allow-list would silently skip cleanup on Arch and leave the daemon running after `pacman -R`.

Add restart-on-upgrade to `postinstall.sh`: `systemctl try-restart tisvlay-daemon.service 2>/dev/null || true` (a no-op when inactive, so harmless on fresh install; picks up the new binary when an upgrade replaced it under a running daemon). The `|| true` is required — `postinstall.sh` runs under `set -e` and every existing `systemctl` call there is already guarded, or an image build / `--installroot` without a running systemd would abort the scriptlet.

## Workstream C — human desktop-user socket access (active-seat ACL + group)

The packaged path still only *prints* `sudo usermod -aG tisvlay $USER`, which is fragile: the user must notice it, run it, and re-login. Internet research (2026) settled the direction. The group-gated `0660` socket on a *non-root* daemon is a sound, mainstream mechanism — the "docker group == root" critique needs a *root* daemon, which we are not, so socket access confers only TV control. And **auto-adding the user in a postinstall is a deliberate, industry-wide refusal**, not an oversight: `$SUDO_USER` is unset under PackageKit / GNOME Software, headless installs have no human, multi-user boxes have no single "the" user, and no distro policy blesses it (wireshark-common creates the group and explicitly does *not* add anyone). No 2026 mechanism auto-ACLs a *socket* to the console user — `uaccess` is device-node-only by construction — so the daemon does it itself.

**Two additive layers** — ORed by the kernel, since a process reaches the socket if it matches *any* entry (the owning group, a named-group entry, or a named-user entry):
- **The `tisvlay` group — the authoritative, admin-facing boundary.** Explicit, durable, works for SSH/headless/service accounts and any admin-chosen user. NixOS `services.tisvlay.users` stays the declarative reference; packaged distros keep `usermod` but *demoted to a documented fallback* for those cases, not the primary desktop UX.
- **A daemon-applied active-seat ACL — the frictionless default for the person at the console.** The daemon grants `u:<active-seat-uid>:rw` on the socket (and `u:<uid>:rx` on the runtime dir), reconciled as the active session changes. This is exactly what `uaccess` does for `/dev` nodes, applied to our socket, reusing the `g:gdm` ACL machinery from Workstream A.

**Active-seat semantics** (logind): a *seat* is one physical workstation (almost always `seat0`); a *session* is one login (graphical, tty, or SSH); the *active* session is the single foreground session on a seat — "who is at the keyboard now." The daemon grants that user. A locked screen stays active (lock ≠ inactive), so the grant persists while locked — load-bearing, since the session service must keep dispatching lock→TV-off. SSH is seatless and never active, so the group covers it. Fast-user-switch moves the ACL to the new active user; because the kernel checks socket permission at `connect()` (not per-message), a client that connected while active stays connected even after being switched away — only a fresh reconnect while backgrounded is denied, a narrow multi-user edge the group also covers.

**Implementation.** Generalize `crates/tisvlay-daemon/src/acl.rs` from "one greeter-group entry" to a computed *set* of entries: the greeter-group entry (Workstream A) plus one active-seat user entry per seat. A background task watches logind — which the daemon already consumes over zbus for lock detection — for active-session changes (`org.freedesktop.login1` Manager `SessionNew`/`SessionRemoved`, each Seat's `ActiveSession` property), recomputes the desired entry set on each change and at bind, and reconciles the socket + runtime dir by rewriting `system.posix_acl_access` (add current entries, drop stale ones, strip back to mode bits when empty). The ACL `mask` covers all named entries (rw on the socket) so the owning `tisvlay` group keeps full effective access alongside the named users — already handled in the Workstream A code. Resolve the active session's uid from logind, *not* from the socket peer, so no `SO_PEERCRED`-through-zenoh plumbing is needed (that path is unavailable — zenoh mediates the socket). Existing clients already use resilient/retry links, which absorb the brief bind-vs-login race (a client that opens before the ACL lands just retries).

**Config surface.** A daemon flag (proposed `--seat-access`, default on in the packaged unit and the NixOS module) enables the active-seat layer; turning it off yields *pure-group* mode for locked-down / kiosk / shared machines where "whoever sits down" should not auto-gain TV control. The greeter flag (`--greeter-access` / `--greeter-group`) stays as-is; both feed the same ACL reconcile. A NixOS option (e.g. `services.tisvlay.seatAccess`, default true) passes the flag; `services.tisvlay.users` remains for explicit grants.

**Security.** The active-seat ACL grants only the person currently at the console — *narrower* than persistent group membership, and dropped when they leave the active seat (uaccess semantics). It grants socket access only (TV control), never the daemon's pairing-credential store, which stays readable by the daemon user alone. The opt-out flag lets an admin fall back to explicit-only grants.

**Precedent (grounding).** The building blocks are systemd's own: in-process ACLs granting a specific uid on a `/run` object (journald per-user journals, coredump, and `/run/credentials/<unit>/`), and active-seat ACLs on device nodes (`uaccess`). Applying the combination to a *socket* is novel — no upstream daemon ACLs its own listening socket (they use group ownership) — but principled, and our in-process-at-bind approach is tidier than the only socket-ACL precedents that exist (nixpkgs `part-db` and the fail2ban-exporter glue, which must poll for the not-yet-created socket). The NixOS module stays clean: it passes a flag; the daemon owns the ACL.

**Unit-enablement convergence (minor, mostly done).** NixOS uses declarative `wantedBy`; packaging uses `systemctl --global enable`. Workstream B fixed the upgrade/removal correctness bug; fuller convergence (systemd presets) is optional and not pursued here.

## Sequencing

1. **A — done.** Daemon greeter-group ACL + per-target wiring; field-validated on Fedora 44 and Ubuntu 26.04, eval-verified on NixOS, same `--greeter-access` unit on Arch; the redundant `usermod`/`gpasswd` and stale docs removed.
2. **B — done.** Upgrade-safe maintainer scripts (guard teardown on `$1`, `try-restart` on upgrade); field-validated on Fedora.
3. **C — designed (this doc); implement next.** Generalize `acl.rs` to a computed entry set, add the logind active-seat watch + reconcile, the `--seat-access` flag (packaged unit + NixOS option), and demote the packaged `usermod` echo to a documented fallback. Reuses A's ACL machinery and the daemon's existing logind zbus link.

## Open decisions

- **C — flag name:** `--seat-access` (parallels `--greeter-access`) vs `--console-access` / `--active-user-access`.
- **C — logind watch granularity:** minimal signal set for active-session changes (`org.freedesktop.login1` Manager `SessionNew`/`SessionRemoved` + each Seat's `ActiveSession` `PropertiesChanged`) vs a coarser periodic reconcile. Prefer signal-driven; keep a reconcile-at-bind seed.
- **C — one reconcile driver:** fold the greeter-group entry and the active-seat user entries into a single logind-driven reconcile of the socket ACL (greeter group is static, seat users are dynamic), rather than two independent appliers.
- **C — multi-seat:** one active-seat user entry per seat (usually just `seat0`); confirm the entry set stays small and correct on multi-seat hosts.
- ~~ACL crate choice~~ — **resolved**: no crate / no `libacl` dependency; the daemon sets `system.posix_acl_access` directly (pure `libc` `setxattr`), resolves groups reentrantly via `getgrnam_r`, and sets the mask explicitly.
- ~~`--greeter-group` vs auto-detect~~ — **resolved**: shipped both — `--greeter-access` (auto-detect `gdm`/`Debian-gdm`, packaged unit) and `--greeter-group <name>` (explicit, NixOS/plasma-login).
- ~~NixOS greeter group + plasma-login handling~~ — **resolved**: `gdm-greeter` accounts are in group `gdm`, `plasmalogin` in group `plasmalogin` (both confirmed in nixpkgs); the module passes `--greeter-group` per DM and the members grant is retired.
- ~~Ubuntu re-test~~ — **resolved**: validated on Ubuntu 26.04, deferral closed.
