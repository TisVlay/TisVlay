# Packaged (deb/rpm) greeter socket access (deferred)

The greeter idle client ships and is wired only on NixOS. Packaging it for `.deb`/`.rpm` is blocked not on the unit — that part is easy — but on giving the login-screen greeter access to the daemon's group-gated socket, which every distro solves differently. Researched 2026-07-19 on an Ubuntu 26.04 GNOME/GDM VM plus upstream sources; filed rather than solved because the clean fix is a daemon change with a security tradeoff, and Ubuntu's model is the one that forces it.

## The easy half — the unit

Ship a `systemd` **user** unit `tisvlay-greeter.service` at `/usr/lib/systemd/user/`, `WantedBy=gnome-session@gnome-login.target` (GDM) / `plasma-login-wayland.target` (KWin), and `systemctl --global enable` it in the maintainer scripts (as the postinstall already does for session/tray). Those targets are active only in the greeter session, so `--global` enablement starts the client only at the login screen, in the greeter user's manager — the packaged analogue of the NixOS `wantedBy`.

## The hard half — socket access

The daemon socket is `tisvlay:tisvlay 0660`; `tisvlay` group membership is the auth boundary. Getting the greeter into that circle depends on how the distro runs the greeter, and the three cases don't share a mechanism:

- **NixOS** — static greeter accounts `gdm-greeter{,-2..5}` (nixpkgs `gdm.nix`). `/etc/group` works; the module adds them to `tisvlay`. **Solved** (shipped).
- **Fedora / RHEL / traditional GDM** — static `gdm` user (uid 42); the DynamicUser greeter ([GNOME gdm #553](https://gitlab.gnome.org/GNOME/gdm/-/work_items/553)) is upstream-in-progress, not shipped there. A postinstall `usermod -aG tisvlay gdm` would work — **to confirm on a current Fedora release**.
- **Ubuntu 26.04** — the greeter is a systemd **`DynamicUser`** (`gdm-greeter`, ephemeral uid in the 60000+ range, not in `/etc/passwd`/`/etc/group`; primary group `gdm`). Both obvious grants are dead ends, confirmed on the VM:
  - **`/etc/group` grant is impossible** — a DynamicUser draws supplementary groups *only* from its unit's `SupplementaryGroups=` ([systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)), which GDM sets to `gdm`; `/etc/group` is never consulted. (A `usermod` test *looked* like it worked only because that ran the greeter under `sudo`, whose `initgroups()` does read `/etc/group` — not the real DynamicUser service path.)
  - **POSIX ACLs are not enforced on `/run`** — a `root:root` file with `g:gdm:rw` (mask `rw`) was still unwritable by `gdm-greeter` though it is in `gdm`, so `setfacl g:gdm:rw` on the socket is a no-op here.

## Recommended approach if picked up

The one handle every greeter shares is the **`gdm` group** (Ubuntu DynamicUser primary group, Fedora static-`gdm` group, grantable on NixOS). Rather than drag the greeter into `tisvlay`, give the daemon a **secondary greeter listen socket owned `tisvlay:gdm 0660`** (e.g. `/run/tisvlay/greeter.sock`), chowned after bind next to the existing `chmod 0660` step. Static-`gdm` and DynamicUser greeters both reach it via their `gdm` group — no group edits, no ACLs. The packaged greeter unit points `--socket` at it; NixOS could adopt it too (retiring its per-user group grant) or keep its working static-user path.

**Security tradeoff (why it is opt-in):** a `tisvlay:gdm` socket is a full daemon session, so it grants any `gdm`-group process TV control — the same exposure as adding `gdm` to `tisvlay`. `gdm` is the display-manager group (root-run, trusted), so this is a modest extension, but gate it behind a daemon `--greeter-socket`/`--greeter-group` flag (and a NixOS/packaging option), off by default.

## Re-evaluation triggers

- A contributor wants the greeter on a packaged (non-Nix) distro — most likely to revive this.
- GDM's DynamicUser greeter ([#553](https://gitlab.gnome.org/GNOME/gdm/-/work_items/553)) lands in Fedora/RHEL, folding the static-`gdm` case into the Ubuntu one (the secondary socket already covers both).
- A cleaner upstream story emerges for granting a greeter session access to a system resource (a standard greeter group, a portal, etc.).
