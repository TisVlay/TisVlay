# GNOME greeter idle: Mutter idle for the login screen

Status: **planned**, not started. Follow-up to the session-service GNOME work ([`gnome-session-events.md`](gnome-session-events.md)); depends on that PR's `idle_mutter` transport. Targets the GDM login greeter the way the session service now targets a GNOME desktop.

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

1. **Move** `idle_mutter.rs` → `tisvlay-core` (feature-gated with the existing wayland module if it is; keep the pure `Tracker` tests). Re-export; update `tisvlay-session` to use `tisvlay_core::idle_mutter`. Session behavior unchanged — verify by the existing session tests + a GNOME re-run.
2. **Extract the selector** (`start_idle_watch` wayland→Mutter) so both consumers share it, or duplicate the ~15-line try-wayland-then-mutter shape in the greeter (it uses `start_idle_watch_on_socket`, a different wayland entry, so a shared selector needs to accept the socket-discovery variant).
3. **Greeter fallback** in `connect_idle_watch`: on wayland failure, attempt `Mutter.IdleMonitor`.
4. **Deployment**: a GDM greeter unit (Nix module first; packaging later) once the manager/target questions above are answered on hardware.

## Verification

Requires being **at the GDM login screen** (the greeter session, `gdm` user, `/run/user/975`) — a logout on the GNOME VM reaches it. Confirm: `Mutter.IdleMonitor` reachable from a process in the greeter session; the greeter publishes `greeter/idle` true/false on idle/resume; a TV blanks at the login screen and wakes on input. The session-service `debug!(?event, "session event")` seam has no greeter analogue — the greeter's own `info!`/publish logs are the audit.

## Non-goals

- No change to the KDE plasma-login greeter path — it keeps `ext-idle-notify`, which works there.
- No new idle vocabulary or greeter protocol — only the acquisition switches transport.
- No attempt to unify with the session's `idle_adapter` selection beyond sharing the transport module; the greeter's socket-discovery wayland entry differs enough that a shared *selector* is optional, not required.
