# Greeter integration generalization

The greeter idle client (`tisvlay-greeter`) — which blanks managed TVs on login-screen idle for OLED burn-in protection — currently supports only KDE's **plasma-login-manager**. The NixOS module now default-enables it when that greeter is the active display manager (`services.tisvlay.greeter.enable = lib.mkDefault config.services.displayManager.plasma-login-manager.enable`). This doc records how (and whether) to extend it to other greeters.

Researched 2026-06-14 against primary sources (compositor git trees, KDE/GNOME GitLab APIs, man pages).

## The two axes

Support is the product of two *independent* requirements, not one:

1. **Idle transport** — the greeter's compositor must speak the `ext-idle-notify-v1` Wayland protocol the client uses.
2. **Session injection** — there must be a way to launch the client into the greeter's session with its `WAYLAND_DISPLAY` / `XDG_RUNTIME_DIR`.

"Is it a Wayland greeter?" predicts neither cleanly — Mutter and Weston are Wayland yet fail axis 1; greetd is Wayland-and-capable yet fails the systemd-drop-in form of axis 2.

## Support matrix (verified)

| Greeter | `ext-idle-notify-v1` | Injection | Verdict |
|---|---|---|---|
| plasma-login-manager (KWin) | ✅ (Plasma 5.27+, registered unconditionally) | `systemd --user` drop-in | ✅ supported today |
| greetd + cage/sway/labwc (wlroots) | ✅ | wrap `default_session.command` | ⭐ best next — binary unchanged |
| GDM (Mutter) | ❌ uses `org.gnome.Mutter.IdleMonitor` D-Bus | `systemd --user` / XDG autostart | 🔴 needs a new client transport |
| SDDM (Weston default) | ❌ internal timer, no client protocol | fragile `CompositorCommand` hack only | 🔴 dead end |
| LightDM (X11) | ❌ no Wayland | — | 🔴 no |

`ext-idle-notify-v1` is spoken by KWin and every wlroots greeter, but **not** Mutter or Weston. Only plasma-login-manager and GDM run the greeter as a real `systemd --user` session (drop-in attachable); SDDM/greetd/LightDM spawn the greeter compositor as a direct child, so injection there means wrapping the greeter command instead.

> Source-hygiene note: `wayland.app`'s protocol pages are a *version tracker*, not a support table — they falsely show Mutter/Weston implementing idle-notify. Verify against compositor source.

## Path forward

### greetd + wlroots (recommended next)

Highest ROI: the compositor already speaks `ext-idle-notify-v1`, so the **existing binary runs unchanged** — only the injection differs. Launch the client as a sibling of the greeter compositor by wrapping the greetd command:

```toml
# /etc/greetd/config.toml
[default_session]
command = "cage -s -- sh -c 'tisvlay-greeter & exec gtkgreet'"
```

The backgrounded client binds the compositor's `WAYLAND_DISPLAY`; it needs to be *connected*, not *visible* (cage runs many clients, shows one). NixOS would wire this into `services.greetd.settings.default_session.command`; non-NixOS ships a documented snippet. This aligns with the established swayidle/hypridle-in-greeter pattern — and the niche (greeter idle → display power for OLED) is otherwise genuinely unfilled across distros.

### GNOME / GDM — sidestep, don't adapt

GDM's greeter is `systemd --user`-attachable, but Mutter lacks `ext-idle-notify-v1`, so the client can't detect idle there without a second transport (the `org.gnome.Mutter.IdleMonitor` D-Bus interface). Prefer **avoiding new client code**: configure the GNOME greeter's own idle-delay so Mutter blanks → DPMS-off, and let the existing daemon-side DPMS connector watcher (`tisvlayd/src/dpms.rs`) drive `on_dpms_off`/`on_dpms_on`. That is greeter-agnostic and needs zero new code — *contingent on verifying Mutter writes the legacy sysfs `dpms` connector attribute the watcher polls* (KWin does; Mutter unverified). If it doesn't, a Mutter-IdleMonitor adapter is a deliberate, larger investment to defer until GNOME demand is real.

### SDDM / LightDM — out of scope

SDDM's default Wayland greeter (Weston) exposes no idle protocol and offers only a fragile `CompositorCommand` injection hack; LightDM is X11. Neither is worth pursuing.

## Shape of the eventual abstraction

Not a "Wayland whitelist" and not a "systemd-target map" — both mispredict (see the matrix). The right model is a small per-greeter **adapter** = an (idle-transport × injection) pair. The existing binary already provides the `ext-idle-notify` transport, so any greeter sharing it (the wlroots family) costs only an injection. This is the login-session slice of the same per-DE adapter direction in [event-driven-power-management.md](event-driven-power-management.md). nixpkgs offers no reusable "run a user service in the greeter session" primitive, so each adapter is bespoke.

## Non-NixOS packaging

Detection is portable — `XDG_SESSION_CLASS=greeter` (logind session class) — but it only *labels* the session; there is no portable *injection* convention. Ship static per-greeter integration files (a `systemd --user` drop-in for plasma-login, inert if that greeter is absent) plus a documented greetd command wrapper. What NixOS uniquely adds is eval-time inference (auto-detecting the active greeter to default-enable), not the mechanism.

## Trigger

Pick up greetd+wlroots support when a user asks for it or the project gains a non-KDE primary target; the GNOME DPMS-watcher verification can fold into the event-driven power-management work.
