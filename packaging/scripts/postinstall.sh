#!/bin/sh
# Post-installation script for tisvlay.
# Creates system user and reloads systemd.

set -e

# Create dedicated system user and group (if they don't exist).
# The daemon runs as this user; state in /var/lib/tisvlay/ is owned
# by it via systemd StateDirectory. The group doubles as the access
# gate: the daemon's zenoh socket in /run/tisvlay/ is 0660
# tisvlay:tisvlay, so every account that should control TVs (desktop
# users) needs tisvlay group membership. The login greeter reaches the
# socket via the daemon's --greeter-access group ACL instead, since the
# modern GDM greeter is a dynamic-userdb account /etc/group can't reach.
if ! getent group tisvlay >/dev/null 2>&1; then
    groupadd --system tisvlay
fi
if ! getent passwd tisvlay >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin \
        --gid tisvlay --comment "tisvlay daemon" tisvlay
fi

# Create state directory (systemd's StateDirectory= creates it on
# service start; pre-creating keeps first-boot ordering forgiving).
install -d -m 0750 -o tisvlay -g tisvlay /var/lib/tisvlay 2>/dev/null || true

# Reload systemd to pick up new service files
systemctl daemon-reload || true

# Refresh the application database so the GUI's new .desktop
# entry shows up in launchers. Redundant on dpkg/rpm (their
# triggers run this automatically) but harmless and useful on
# minimal distros without those triggers configured.
update-desktop-database /usr/share/applications 2>/dev/null || true

# Enable (but don't start) the system daemon.
systemctl enable tisvlay-daemon.service 2>/dev/null || true

# On upgrade, restart the daemon so it runs the new binary — the old
# package's preremove skips its stop on upgrade, so a running daemon would
# otherwise keep the old code. try-restart is a no-op when inactive, so a
# fresh install (daemon enabled but not started) is unaffected.
systemctl try-restart tisvlay-daemon.service 2>/dev/null || true

# Enable the per-user session + tray for every user's graphical session
# (mirrors the NixOS module's wantedBy=graphical-session.target). These
# are user units, so a package script can't touch a live user manager;
# --global installs the enablement symlinks under /etc/systemd/user, and
# each user's next graphical login picks them up. They start only inside a
# graphical session and no-op without socket access, so enabling globally
# is safe for users not in the tisvlay group.
systemctl --global enable tisvlay-session.service tisvlay-tray.service 2>/dev/null || true

# GDM greeter idle client (OLED protection at the login screen). Its unit
# hangs off gnome-session@gnome-login.target, active only inside a GDM
# greeter session, so the --global symlink is inert under other display
# managers. The greeter reaches the socket through the daemon's
# greeter-group ACL (tisvlay-daemon --greeter-access) — no group edit here,
# since the modern GDM greeter is a dynamic-userdb account /etc/group can't
# reach.
systemctl --global enable tisvlay-greeter-gdm.service 2>/dev/null || true

echo "tisvlay installed. Grant yourself socket access and start the daemon:"
echo "  sudo usermod -aG tisvlay \$USER   # re-login to take effect"
echo "  sudo systemctl start tisvlay-daemon"
echo "  tisvlay pair \"My TV\" --vendor lg --json '{\"ip\":\"<address>\"}'"
echo "The session + tray start automatically on your next graphical login;"
echo "start them now without re-login with:"
echo "  systemctl --user start tisvlay-session tisvlay-tray"
