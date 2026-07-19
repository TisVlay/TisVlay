# Packaged greeter socket access: GDM's DynamicUser greeter (Ubuntu) (deferred)

The greeter idle client is wired on NixOS (`services.tisvlay.greeter.enable`) and, in the `.deb`/`.rpm`/`.pkg.tar.zst` packages, for the **static-`gdm`** display managers (Fedora/RHEL, Arch). What remains deferred is the one greeter that can't be granted socket access the ordinary way: **GDM running the greeter as a systemd `DynamicUser`**, which Ubuntu 26.04 ships. Researched 2026-07-19 on an Ubuntu 26.04 GNOME/GDM VM plus upstream sources.

## What ships now (solved)

The daemon socket is `tisvlay:tisvlay 0660`; `tisvlay` group membership is the auth boundary. Getting the greeter into that circle depends on how the distro runs it:

- **NixOS** — static greeter accounts `gdm-greeter{,-2..5}` (nixpkgs `gdm.nix`); the module lists them in the `tisvlay` group's members. Plasma-login and GDM both wired via `services.tisvlay.greeter.enable`.
- **Fedora / RHEL / Arch (static `gdm`)** — the greeter runs as the static `gdm` system user, so `/etc/group` is honored at session start. The package ships a GDM greeter unit (`tisvlay-greeter-gdm.service`, `WantedBy=gnome-session@gnome-login.target` — GDM's greeter-only attach point), `systemctl --global enable`s it, and the postinstall runs `usermod -aG tisvlay gdm` (guarded on the `gdm` user existing; the preremove reverses it). A greeter that still can't reach the socket (grant not yet effective, or the wrong DM) exits cleanly rather than restart-looping — the client pre-flights the socket and treats `EACCES` as "not my session."

## What's still deferred — Ubuntu's DynamicUser greeter

Ubuntu 26.04 runs the GDM greeter as a systemd **`DynamicUser`** (`gdm-greeter`, ephemeral uid in the 60000+ range, not in `/etc/passwd`/`/etc/group`; primary group `gdm`). Both obvious grants are dead ends, confirmed on the VM:

- **`/etc/group` grant is impossible** — a DynamicUser draws supplementary groups *only* from its unit's `SupplementaryGroups=` ([systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)), which GDM sets to `gdm`; `/etc/group` is never consulted. The shipped `usermod -aG tisvlay gdm` adds the static `gdm` *daemon* user, which the DynamicUser greeter does not run as — so on Ubuntu the grant is an inert no-op and the greeter's socket pre-flight makes it exit cleanly instead of looping.
- **POSIX ACLs are not enforced on `/run`** — a `root:root` file with `g:gdm:rw` (mask `rw`) was still unwritable by `gdm-greeter` though it is in `gdm`, so `setfacl g:gdm:rw` on the socket is a no-op here.

## Recommended approach if picked up

The one handle every greeter shares is the **`gdm` group** (Ubuntu DynamicUser primary group, Fedora/Arch static-`gdm` group). Rather than drag the greeter into `tisvlay`, give the daemon a **secondary greeter listen socket owned `tisvlay:gdm 0660`** (e.g. `/run/tisvlay/greeter.sock`), chowned after bind next to the existing `chmod 0660` step. DynamicUser and static-`gdm` greeters both reach it via their `gdm` group — no group edits, no ACLs. The packaged GDM greeter unit would point `--socket` at it, and the static-`gdm` distros could retire their `usermod` grant in favor of it.

**Security tradeoff (why it is opt-in):** a `tisvlay:gdm` socket is a full daemon session, so it grants any `gdm`-group process TV control — the same exposure as the shipped `usermod -aG tisvlay gdm` on static-`gdm` distros. `gdm` is the display-manager group (root-run, trusted), so this is a modest extension, but gate it behind a daemon `--greeter-socket`/`--greeter-group` flag (and a NixOS/packaging option), off by default.

## Re-evaluation triggers

- A contributor wants the greeter working on Ubuntu (or any GDM that runs the greeter as a DynamicUser) — most likely to revive this.
- GDM's DynamicUser greeter ([GNOME gdm #553](https://gitlab.gnome.org/GNOME/gdm/-/work_items/553)) lands in Fedora/RHEL, folding the static-`gdm` case into the Ubuntu one (the secondary socket already covers both).
- A cleaner upstream story emerges for granting a greeter session access to a system resource (a standard greeter group, a portal, etc.).
