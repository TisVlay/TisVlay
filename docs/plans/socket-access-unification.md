# Socket-access unification + upgrade-safe packaging

Move socket-access policy for the login greeter into the daemon (a `gdm`-group ACL applied at bind), and fix the maintainer scripts so upgrades don't tear down the running stack. Both came out of a field session on a Fedora 44 GNOME/GDM VM plus upstream research, and together they let us delete a pile of per-distro grant hacks that were either broken or divergent.

## Motivation

The greeter socket-access mechanism we shipped (`usermod -aG tisvlay gdm` in the rpm/deb postinstall) is **wrong on every modern GDM** and only ever worked in theory on the retired static-`gdm` model. Testing on the VM also shook out two packaging-lifecycle bugs unrelated to the greeter. Rather than patch each distro's grant separately, the research points at one mechanism that is correct everywhere — a daemon-applied group ACL — which also collapses the NixOS/packaged divergence.

## Findings (grounding; all validated this session unless noted)

Modern GDM (GNOME 49+: Fedora 43/44, Ubuntu 25.10+) runs the greeter as a systemd-userdb **dynamic** user `gdm-greeter`, uid in the 60578–60705 range, home under `/run/gdm/`, **primary group `gdm`** — not the static `gdm` daemon user (uid 42). A dynamic/userdb account draws no supplementary groups from `/etc/group`, so `usermod -aG tisvlay gdm` targets the wrong account and never reaches the greeter (confirmed: the greeter EACCES-exits). Upstream GDM solved the same problem for its own polkit/D-Bus policies in 2025 by keying on the **`gdm` group** (`subject.isInGroup("gdm")`), because that group is the one stable handle across the migration.

A `g:gdm` **group ACL** on the socket (`g:gdm:rw`) plus `g:gdm:rx` on the runtime dir lets the dynamic greeter connect — validated live: the greeter got past its pre-flight, opened its zenoh session, and read the daemon's timeout key. This also corrects the earlier `docs/concerns/greeter-packaged-socket-access.md` claim that "ACLs are not enforced on /run": that was a *user*-vs-*group* ACL misdiagnosis (tmpfs honors ACLs; a group ACL keyed on `gdm` works because `gdm` is the greeter's primary group).

NixOS is a third case: `nixos/modules/services/display-managers/gdm.nix` pre-allocates the greeter as **static** accounts `gdm-greeter{,-2..5}` at uid 60578+ (static-emulating the dynamic greeter). Our `users.groups.tisvlay.members` grant works there only because those accounts are static (`/etc/group` honored) — a different mechanism from the packaged path, for the same end.

Two lifecycle bugs (independent of the greeter):
- **Maintainer scripts don't distinguish upgrade from removal.** On an rpm upgrade the scriptlet order is new-`%post` → old `%preun`, so `preremove.sh` (which unconditionally stops + disables) runs *after* `postinstall.sh` enabled things and undoes them: the daemon is left stopped and units disabled. Reproduced via `dnf reinstall`.
- **No restart-on-upgrade** — even once the above is fixed, an upgrade needs an explicit restart or it keeps running the old binary.

Already landed this session as defensive context: the greeter pre-flights the socket and exits clean on `EACCES` (a stray-enabled greeter is inert, never loops), and the tray survives a missing StatusNotifier host instead of crash-looping (`assume_sni_available`). Both stand regardless of this plan.

## Workstream A — daemon-applied `gdm`-group ACL for greeter access

At the same post-bind step that already `chmod`s the socket to `0660`, additionally apply a POSIX ACL granting the `gdm` group access: `g:gdm:rw` on the socket and `g:gdm:rx` on the runtime directory (the greeter must traverse the `0750` dir *and* write the socket). Resolve `gdm` by group **name** (its gid varies by distro — 42 on Fedora, dynamic elsewhere; Debian may rename it `_gdm`), and skip cleanly when the group is absent (non-GDM systems). Do it in-process, not via a shell `setfacl` (per the repo's no-shell-tooling convention) — via an ACL crate (candidates: `exacl`, `posix-acl`) resolving the group with `getgrnam`.

Gate it behind an explicit daemon option (proposed `--greeter-group <name>`, unset by default) rather than always-on, since it widens socket reach from the `tisvlay` group to also the `gdm` group. `gdm` is the root-run, trusted display-manager group, so the exposure is modest and is the same tradeoff `docs/concerns/greeter-packaged-socket-access.md` already accepted — but keeping it opt-in means a daemon with no greeter deployed grants nothing extra.

This lets us **delete**: the rpm/deb `usermod -aG tisvlay gdm` + `gpasswd -d` lines; the NixOS `users.groups.tisvlay.members` greeter grant (the module's `greeter.enable` instead passes `--greeter-group gdm` to the daemon unit); and the concern doc's "secondary `tisvlay:gdm` socket" recommendation (a plain ACL on the main socket suffices). It **closes the Ubuntu DynamicUser deferral** — the same ACL is what makes Ubuntu work — so `docs/concerns/greeter-packaged-socket-access.md` retires into a short "solved via group ACL" note.

## Workstream B — upgrade-safe maintainer scripts

Guard the removal-only logic in `preremove.sh` on the upgrade/removal signal, in a form that works for the single script nfpm shares across deb/rpm/arch: rpm passes a numeric `$1` (`0` = final removal, `≥1` = upgrade); deb passes a string (`remove`/`purge` vs `upgrade`). So the stop/disable runs only when `[ "$1" = 0 ] || [ "$1" = remove ] || [ "$1" = purge ]`, and is skipped on `1`/`upgrade`.

Add restart-on-upgrade to `postinstall.sh`: `systemctl try-restart tisvlay-daemon.service` (a no-op when inactive, so harmless on fresh install; picks up the new binary when an upgrade replaced it under a running daemon). This is the standard `%systemd_postun_with_restart` behavior, hand-rolled because nfpm ships generic scripts rather than rpm macros.

## Workstream C — NixOS ↔ packaging convergence

With A, greeter access is one mechanism everywhere and the module shrinks. The remaining divergences to weigh (not necessarily close now):
- **Human-user socket grant.** Packaging only *prints* `usermod -aG tisvlay $USER` (the user flagged this as fragile); NixOS has declarative `services.tisvlay.users`. Options: extend the daemon's ACL model to a configurable list of grantee groups/users applied at bind (so both paths drive the same daemon logic); auto-grant the primary graphical user via logind in postinstall (less reliable — `$SUDO_USER` isn't guaranteed in a dnf scriptlet); or keep it manual with better docs. Decision deferred.
- **Unit enablement.** NixOS uses declarative `wantedBy`; packaging uses `systemctl --global enable` in scripts. Workstream B fixes the correctness bug here; fuller convergence (systemd presets) is optional.

## Sequencing

1. **A** first — it is validated, deletes the most cruft, and is the correctness fix for the greeter grant we already shipped.
2. **B** — small, self-contained lifecycle correctness; can land alongside or just after A.
3. **C** — design-heavier; revisit once A/B are in and the shape is clear.

## Open decisions

- ACL crate choice (`exacl` vs `posix-acl` vs a thin `getgrnam` + `nix`/`libc` `acl_*` binding) and its portability/build weight.
- The daemon config surface: `--greeter-group gdm` explicit flag (proposed) vs auto-detect a present `gdm` group. Explicit keeps it opt-in.
- Confirm the NixOS static `gdm-greeter` accounts carry group `gdm` (so the same ACL, not the members grant, can drive them) — quick nixpkgs check before deleting the module grant.
- Whether Workstream C's human-user grant folds into the daemon ACL model or stays out of scope.
