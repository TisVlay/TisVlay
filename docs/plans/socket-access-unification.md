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
- **Ubuntu:** re-test the ACL on Ubuntu (per Findings) before touching the Ubuntu deferral in the concern doc.
- **Upgrade hygiene:** hosts that installed a pre-ACL package added `gdm` to the `tisvlay` group; since removal-time `gpasswd -d` is going away, add a one-time `gpasswd -d gdm tisvlay` in `postinstall` so the obsolete standing membership is revoked when moving to the ACL model.

Security: the ACL widens socket reach from the `tisvlay` group to also the greeter group (`gdm`), a root-run, trusted display-manager group. It stays opt-in (unset by default) so a daemon with no greeter deployed grants nothing extra; the reconcile step above is what makes "opt-in" actually revocable.

## Workstream B — upgrade-safe maintainer scripts

Guard the removal-only logic in `preremove.sh` so it runs **only on real removal**, in a form correct for the single script nfpm shares across deb/rpm/arch — noting the three managers use three different `$1` conventions:
- rpm `%preun`: numeric count, `0` = final removal, `≥1` = upgrade/reinstall.
- deb `prerm`: string, `remove`/`purge` = removal, `upgrade`/`failed-upgrade` = upgrade.
- pacman `pre_remove`: the package **version string** (e.g. `0.3.0-1`), and pacman calls `pre_remove` **only** on removal (upgrades use `pre_upgrade`).

A guard that matches removal on all three by *excluding upgrades* rather than listing removals: `case "$1" in 1|upgrade|failed-upgrade) : skip ;; *) <stop/disable> ;; esac`. This skips on rpm `1`, deb `upgrade`, and never wrongly skips on pacman (version-string `$1` falls to the default → cleanup runs, which is correct since pacman only reaches `pre_remove` on removal). A naive `[ "$1" = 0 ] || [ "$1" = remove ]` allow-list would silently skip cleanup on Arch and leave the daemon running after `pacman -R`.

Add restart-on-upgrade to `postinstall.sh`: `systemctl try-restart tisvlay-daemon.service 2>/dev/null || true` (a no-op when inactive, so harmless on fresh install; picks up the new binary when an upgrade replaced it under a running daemon). The `|| true` is required — `postinstall.sh` runs under `set -e` and every existing `systemctl` call there is already guarded, or an image build / `--installroot` without a running systemd would abort the scriptlet.

## Workstream C — NixOS ↔ packaging convergence

With A, greeter access is one mechanism wherever the greeter group is configured, and the module shrinks. Remaining divergences to weigh (not necessarily close now):
- **Human-user socket grant.** Packaging only *prints* `usermod -aG tisvlay $USER` (the user flagged this as fragile); NixOS has declarative `services.tisvlay.users`. Options: extend the daemon's ACL model to a configurable list of grantee groups/users applied at bind (which would also subsume `--greeter-group` — mark that flag provisional against this); or auto-grant the primary graphical user in postinstall. The auto-grant's real weakness is that **no graphical session may exist at install time** for a logind lookup to find (a `$SUDO_USER`-based variant has a *different* problem — `$SUDO_USER` isn't set under a dnf/PackageKit scriptlet; don't conflate the two). Decision deferred.
- **Unit enablement.** NixOS uses declarative `wantedBy`; packaging uses `systemctl --global enable` in scripts. Workstream B fixes the correctness bug here; fuller convergence (systemd presets) is optional.

## Sequencing

1. **A** first, but staged behind its preconditions: land the daemon ACL + wire `--greeter-group` into the packaged unit *before* deleting any grant, and delete per target (Fedora, then NixOS after the group check + plasma-login handling, then Ubuntu after re-test). It is the correctness fix for the greeter grant we already shipped.
2. **B** — small, self-contained lifecycle correctness; can land alongside or just after A.
3. **C** — design-heavier; revisit once A/B are in and the shape is clear.

## Open decisions

- ACL crate choice (must use `getgrnam_r` under the hood): `exacl` vs `posix-acl` vs a thin `getgrnam_r` + `nix`/`libc` `acl_*` binding — weigh portability/build weight and whether it lets us set the mask explicitly.
- The daemon config surface: `--greeter-group <name>` explicit flag (proposed, provisional against Workstream C's grantee-list) vs auto-detecting a present `gdm`/`Debian-gdm` group. Explicit keeps it opt-in and lets plasma-login pass its own group.
- Confirm the NixOS static `gdm-greeter` accounts carry group `gdm`, and decide how the plasma-login `plasmalogin` greeter (not in `gdm`) is granted, before deleting the module members grant.
- Whether to re-test on Ubuntu now (to close the deferral) or leave it deferred until the ACL lands on a target we control.
