# User service auto-start after `nixos-rebuild switch`

NixOS's activation script restarts **system** services on `switch`, but
not **user** services — those only pick up changes on next login. Other
projects (e.g. NordVPN) work around this by having their desktop GUI
detect that the companion user service isn't running and start it on
demand.

Options for TisVlay:

1. **D-Bus activation for the user session** — ship an
   `org.tisvlay.Session1.service` D-Bus user service file so that when
   `tisvlay-tray` (or anything else) calls the session interface on the
   user bus, systemd auto-starts `tisvlay-session`.

2. **Tray/GUI starts the session service** — have `tisvlay-tray` (or the
   future `tisvlay-gui`) call `systemctl --user start tisvlay-session`
   when it detects the service isn't running. This is closest to what
   NordVPN's desktop client does.

3. **`restartTriggers` in the NixOS module** — add a trigger so the
   activation script kicks user services after `switch`. Fragile because
   it requires enumerating logged-in sessions via `loginctl`.

Option 2 is the most natural fit given the `tisvlay-gui` roadmap — the
GUI/tray becomes the entry point that ensures the rest of the user-side
stack is running.
