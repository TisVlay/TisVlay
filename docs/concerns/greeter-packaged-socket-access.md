# Packaged greeter socket access (solved via a daemon group ACL)

The login-screen greeter reaches the daemon socket through a **daemon-applied POSIX group ACL**, not group membership. At bind, after the `chmod 0660`, the daemon adds `g:<greeter-group>:rw` on the socket and `g:<greeter-group>:rx` on the runtime directory (`crates/tisvlay-daemon/src/acl.rs`, in-process via the `system.posix_acl_access` xattr — no libacl dependency). The packaged `tisvlay-daemon.service` passes `--greeter-access`, which auto-detects the group from `gdm` / `Debian-gdm`; NixOS or a plasma-login setup can pass `--greeter-group <name>` explicitly.

## Why membership doesn't work

Modern GDM (GNOME 49+: Fedora 43/44, Ubuntu 25.10+) allocates the greeter through systemd-userdb as a **dynamic** user `gdm-greeter` (uid in the 60578–60705 range, home under `/run/gdm/`, **primary group `gdm`**) — not the static `gdm` daemon user. A userdb/dynamic account draws no supplementary groups from `/etc/group`, so `usermod -aG tisvlay gdm` targets the wrong account and never reaches the greeter. Keying on the **`gdm` group** is how GDM's own polkit/D-Bus policies gate the greeter (since 2025), and it is the one handle stable across the static-account (NixOS) and dynamic (Fedora/Ubuntu) greeters alike.

Two claims in this doc's earlier history were wrong and are corrected here:
- Fedora/RHEL/Arch are **not** the "static `gdm` greeter" — Fedora 44 runs the dynamic userdb greeter (confirmed on the VM: `usermod -aG tisvlay gdm` left the greeter EACCES-exiting).
- POSIX ACLs **are** honored on `/run` tmpfs. The earlier "not enforced" note was a *user*-vs-*group* ACL misdiagnosis — a `g:gdm` group ACL works because `gdm` is the greeter's primary group (validated live).

The old "secondary `tisvlay:gdm` socket" idea is therefore unnecessary: a plain ACL on the main socket suffices.

## Validation status

- **Fedora 44 (dynamic greeter): validated.** The daemon applies the ACL under its full systemd sandbox (journal: "greeter-group socket ACL applied"; `getfacl` shows `group gdm rw-` with a matching mask, so `tisvlay`-group clients keep access), and the dynamic `gdm-greeter` connects — no `usermod`, no manual `setfacl`.
- **Remaining, tracked in [docs/plans/socket-access-unification.md](../plans/socket-access-unification.md):** wire `--greeter-group` into the NixOS module (confirming the static `gdm-greeter` accounts carry group `gdm`, and handling the plasma-login `plasmalogin` greeter, which is not in `gdm`), and re-test on Ubuntu before treating it as covered there.

## Security

The ACL widens socket reach from the `tisvlay` group to also the greeter group (`gdm`), the root-run, trusted display-manager group. It is opt-in — the daemon grants nothing extra unless `--greeter-access`/`--greeter-group` is set, and the ACL is reconciled (stripped back to mode bits) on every start when it isn't.
