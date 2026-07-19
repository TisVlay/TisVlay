# GNOME greeter idle: Mutter idle for the login screen

Status: **in progress** (roped into the session-events PR). Builds on that PR's `idle_mutter` transport. Targets the GDM login greeter the way the session service now targets a GNOME desktop.

## Approach validation (2026-07-19 research)

Checked upstream before committing, to avoid building on a legacy interface:

- **Mutter still ships no `ext-idle-notify-v1`** (GNOME 51, 2026) — only KWin and wlroots do, and there is no roadmap indication GNOME will. So `Mutter.IdleMonitor` is the only GNOME idle option, for the session and the greeter alike.
- **`org.gnome.Mutter.IdleMonitor` is current, stable, and not deprecated** — its Mutter API docs are generated for Version 51 with no deprecation notice (a `final class`), and Firefox, Chromium, phosh, and gnome-desktop all depend on it, so GNOME cannot quietly drop it.
- **The wayland-first ordering is the hedge**: if Mutter ever adopts `ext-idle-notify`, both the session and the greeter automatically prefer it and the D-Bus path goes dormant — no legacy lock-in.
- **GNOME dropped Mutter's X11 backend (Nov 2025)**, making logind `Type=x11` vestigial — which is why the lock watcher's seat-based (not `Type`-based) session selection is the durable choice.
- **GDM deployment is feasible**: on the VM, `gdm` gets a systemd user manager (`user@.service` + `gnome-session-manager@` + `graphical-session.target`), and GDM-scoped drop-ins already exist (`70-gdm-skip.conf`), so a `systemd.user` greeter unit gated with `ConditionUser=gdm` is workable.

## Problem

`tisvlay-greeter` publishes `greeter/idle` so a TV blanks/wakes with the *login screen's* idle, not just a logged-in desktop's. It uses only the `ext-idle-notify-v1` Wayland protocol (`start_idle_watch_on_socket`, discovering the `wayland-N` socket under `XDG_RUNTIME_DIR` because the greeter unit carries no `WAYLAND_DISPLAY`). That works under KDE's plasma-login greeter (compositor is KWin, which advertises the protocol) — the only greeter wired today ([`plasma-login-wayland.target`](../../packaging/nix/module.nix)). Under **GDM** the greeter is gnome-shell/Mutter, which does not advertise `ext_idle_notifier_v1` (the same gap the session service just closed), so `connect_idle_watch` exhausts its retries and the login screen never drives idle. GNOME's idle API is the D-Bus `org.gnome.Mutter.IdleMonitor`, exposed on the greeter's own (the `gdm` user's) session bus while the login screen is up.

## Design

Reuse the transport the session service already proved on GNOME, and give the greeter the same wayland-or-Mutter selection.

- **Shared transport.** Move `idle_mutter` out of `tisvlay-session` into `tisvlay-core` beside `wayland.rs` (both are idle transports over the shared `IdleNotification`/`IdleTransition` vocabulary; `tisvlay-core` already owns the wayland one and the greeter already depends on it). The session's `idle_adapter` and the greeter both call the same selector. No behavior change for the session — just a move + re-export.
- **Greeter fallback.** `connect_idle_watch` tries the discovered wayland socket first (KDE/wlroots greeters), then `Mutter.IdleMonitor` (GDM). The greeter's single idle notification (`tag: 0`, `timeout_ms`) maps onto either transport unchanged; the greeter already consumes a `(tag, IdleTransition)` channel and holds one handle for the process lifetime, so only the acquisition switches.
- **Bus discovery.** The greeter unit lacks `WAYLAND_DISPLAY` (hence socket discovery) and likely lacks `DBUS_SESSION_BUS_ADDRESS`. `zbus::Connection::session()` falls back to `$XDG_RUNTIME_DIR/bus`, which the greeter unit does inherit, so the D-Bus path should need no manual discovery — **to confirm on a live GDM greeter**, since if it does not, the Mutter transport needs a socket-path fallback mirroring the wayland one.

## Deployment (the GDM-specific unknown)

The KDE greeter attaches a `systemd.user` unit to a vendor-shipped, greeter-only target (`plasma-login-wayland.target`) via an `asDropin` `Wants=`, running as the greeter user, which joins the `tisvlay` group for socket access. GDM has **no equivalent named user target**: GDM launches gnome-shell for the `gdm` user directly, and the greeter session's runtime dir (`/run/user/975`) exists only at the login screen. Open questions to resolve on a real GDM box:

- Does GDM start a systemd **user manager** for `gdm` (so a `systemd.user` unit can attach at all), and what target is greeter-scoped there — `graphical-session.target` under the `gdm` manager is greeter-only (gdm has no "real" sessions), unlike a normal user where it is every session? If so, `wantedBy = graphical-session.target` under the gdm manager is correct — but the module's `systemd.user.services` apply to *all* users, so scoping the unit to only the `gdm` manager needs a gdm-specific mechanism (a drop-in keyed on the gdm user, or a system unit template).
- Socket access: `gdm` must join the `tisvlay` group (mirrors `greeter.user`), and the daemon socket (`/run/tisvlay/zenoh.sock`) must be reachable from the greeter session.
- The `.deb`/`.rpm` packaging has no greeter unit at all today (Nix-only); a GDM greeter unit would be the first packaged greeter.

## Implementation sketch

1. **Move** `idle_mutter.rs` → `tisvlay-core` beside `wayland.rs` under a `mutter-idle` feature (implies `wayland` for the shared `IdleNotification`/`IdleTransition` vocab, adds `zbus`/`futures-util`); the pure `Tracker` tests move with it. `tisvlay-session` uses `tisvlay_core::idle_mutter`; behavior unchanged. **Done** (`refactor(core): share the Mutter idle transport`).
2. **Greeter fallback** in `connect_idle_watch`: try the wayland socket first, and on `ProtocolNotSupported` (socket up, no `ext-idle-notify`) fall back to `Mutter.IdleMonitor` — distinguished from socket-not-ready (retry) via a typed outcome so a GNOME greeter doesn't burn the retry budget. **Done** (`feat(greeter): fall back to Mutter idle on GNOME greeters`).
3. **NixOS deployment**: generalize `greeter.enable` to GDM. The GDM greeter runs `gnome-session --session=gnome-login`, so `gnome-session@gnome-login.target` is its greeter-only session target — the analogue of `plasma-login-wayland.target`; the unit hangs off it (`wantedBy`/`partOf`) and starts only at the login screen. GDM's greeter accounts (`gdm-greeter{,-2,-3,-4,-5}`) join the `tisvlay` group for socket access. **Done** (Nix module).
4. **Debian/rpm greeter unit**: the remaining follow-up — see [Packaged greeter (deb/rpm)](#packaged-greeter-debrpm) below for the researched approach.

## Packaged greeter (deb/rpm)

The `.deb`/`.rpm` ship no greeter unit today (the greeter is Nix-only). Packaging one for GDM has two parts: the unit, and — the hard part — giving the greeter socket access. Researched 2026-07-19 on the Ubuntu 26.04 GNOME/GDM VM plus upstream sources.

### The unit (straightforward)

Ship a `systemd` **user** unit `tisvlay-greeter.service` in the package at `/usr/lib/systemd/user/`, `WantedBy=gnome-session@gnome-login.target`, `ExecStart` the greeter. Enable it with `systemctl --global enable tisvlay-greeter.service` in the maintainer scripts (as the postinstall already does for the session/tray). Because `gnome-session@gnome-login.target` is active only in the greeter session, the `--global` enablement starts it only at the login screen, in the greeter user's manager — the packaged analogue of the NixOS `wantedBy` on that target.

### The socket-access problem (the real work)

The daemon socket is `tisvlay:tisvlay 0660`; `tisvlay` group membership is the auth boundary. The greeter isn't (and can't easily be) in that group, and the fix differs by how the distro runs the greeter:

- **NixOS** — static greeter accounts `gdm-greeter{,-2..5}` (nixpkgs `gdm.nix`). `/etc/group` membership works; the module adds them to `tisvlay`. **Solved.**
- **Fedora/RHEL / traditional GDM** — the greeter runs as the static `gdm` user (uid 42); the DynamicUser greeter ([GNOME gdm #553](https://gitlab.gnome.org/GNOME/gdm/-/work_items/553)) is upstream-in-progress, not shipped there. A postinstall `usermod -aG tisvlay gdm` works.
- **Ubuntu 26.04** — the greeter is a **systemd `DynamicUser`** (`gdm-greeter`, ephemeral uid in the 60000+ range, *not* in `/etc/passwd`/`/etc/group`; primary group `gdm`). Two dead ends, both confirmed on the VM:
  - **`/etc/group` grant is impossible** — a DynamicUser gets supplementary groups *only* from its unit's `SupplementaryGroups=` ([systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)), and GDM sets that to `gdm`; `/etc/group` is never consulted (the earlier `usermod` "success" was `sudo`'s `initgroups()`, not the real service path). Our unit is a *user* unit, so it can't add groups either (unprivileged manager).
  - **POSIX ACLs are not enforced on `/run`** — a `root:root` plain file with `g:gdm:rw` (mask `rw`) is still unwritable by `gdm-greeter` though it is in `gdm`. So `setfacl g:gdm:rw` on the socket does nothing here.

### Recommended approach: a secondary greeter socket

The one handle every greeter shares is the **`gdm` group** (Ubuntu DynamicUser primary group, Fedora static-`gdm` group, and grantable on NixOS). So instead of dragging the greeter into the `tisvlay` group, give the daemon a **secondary listen socket for the greeter, owned `tisvlay:gdm 0660`** (e.g. `/run/tisvlay/greeter.sock`), added next to the existing chmod-after-bind step. The greeter connects there; static-`gdm` and DynamicUser greeters both reach it via their `gdm` group, with no group edits or ACLs. The packaged greeter unit points `--socket` at it; NixOS can adopt it too (retiring the per-user group grant) or keep its working static-user path.

Security note: a `tisvlay:gdm` socket is a full daemon session, so it grants any `gdm`-group process TV control — the same exposure as adding `gdm` to `tisvlay`. `gdm` is the display-manager group (root-run, trusted), so this is a modest, opt-in extension (gate it behind a daemon `--greeter-group`/`--greeter-socket` flag or a NixOS/packaging option, off by default).

### Open questions

- Confirm Fedora/RHEL still runs the greeter as static `gdm` (not DynamicUser) on the target release; if it flips to DynamicUser, it folds into the Ubuntu case (the secondary socket already covers it).
- Whether to make the secondary greeter socket the *uniform* greeter transport (all distros) or keep NixOS on its static-user + `tisvlay`-group path.
- The zenoh mechanics of a second unix listen endpoint on the daemon router (config + post-bind chown to `tisvlay:gdm`).

## Verification (2026-07-19, on the Ubuntu 26.04 GNOME/GDM VM, at the login screen)

The critical unknown — a process in the GDM greeter session reaching Mutter — is confirmed:

- The greeter session is a logind `Class=greeter`, `Type=wayland`, seat0 session, run as `gdm-greeter` (Ubuntu allocates its uid dynamically — 60580 this boot; `/run/user/60580`, `wayland-0`, own session bus).
- `busctl ... org.gnome.Mutter.IdleMonitor GetIdletime` as `gdm-greeter` returned a live idle time — **Mutter's idle monitor is reachable in the greeter session**.
- The zigbuilt `tisvlay-greeter`, run as `gdm-greeter` (temporarily granted the `tisvlay` group), connected to the daemon socket and published `greeter/idle` presence, then stopped at "nothing to watch" only because `greeter/timeout=0` (no automated TV met the daemon's greeter-timeout gate at that moment — orthogonal to the greeter idle code).
- `gnome-session@gnome-login.target` is **active** in the greeter's user manager (the deployment attach point), and the daemon socket is `tisvlay:tisvlay 0660`, so the greeter account must join `tisvlay` (it does, and then reaches the socket).

The NixOS module change is eval-checked (a `services.displayManager.gdm.enable` config resolves the greeter unit's `wantedBy gnome-session@gnome-login.target` and adds the `gdm-greeter*` accounts to the group). The remaining end-to-end field check — a TV actually blanking at the GDM login screen — needs a non-zero greeter timeout (an automated TV with `idle_timeout_seconds > 0`) and a deployed unit.

## Non-goals

- No change to the KDE plasma-login greeter path — it keeps `ext-idle-notify`, which works there.
- No new idle vocabulary or greeter protocol — only the acquisition switches transport.
- No attempt to unify with the session's `idle_adapter` selection beyond sharing the transport module; the greeter's socket-discovery wayland entry differs enough that a shared *selector* is optional, not required.
