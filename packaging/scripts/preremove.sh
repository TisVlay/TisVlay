#!/bin/sh
# Pre-removal script for tisvlay.
# Stops and disables services before package removal.

set -e

# Stop and disable system daemon
systemctl stop tisvlay-daemon.service 2>/dev/null || true
systemctl disable tisvlay-daemon.service 2>/dev/null || true

# Remove the global enablement of the per-user units (installed by
# postinstall). Live user managers still running them are left alone —
# they stop with their graphical session; --global only drops the
# /etc/systemd/user symlinks so new logins don't restart them.
systemctl --global disable tisvlay-session.service tisvlay-tray.service 2>/dev/null || true

# Undo the GDM greeter enablement and the group grant postinstall added.
systemctl --global disable tisvlay-greeter-gdm.service 2>/dev/null || true
if getent group tisvlay >/dev/null 2>&1 && getent passwd gdm >/dev/null 2>&1; then
    gpasswd -d gdm tisvlay >/dev/null 2>&1 || true
fi

# Reload systemd
systemctl daemon-reload || true

# Note: the tisvlay user/group and /var/lib/tisvlay/ are intentionally
# preserved on removal — they contain TV credentials. The user can
# manually remove them with:
#   sudo userdel tisvlay
#   sudo groupdel tisvlay
#   sudo rm -rf /var/lib/tisvlay
