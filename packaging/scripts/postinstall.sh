#!/bin/sh
# Post-installation script for tisvlay.
# Creates system user, reloads systemd and D-Bus.

set -e

# Create dedicated system user and group (if they don't exist).
# The daemon runs as this user; config in /var/lib/tisvlay/ is
# owned by this user via systemd StateDirectory.
if ! getent group tisvlay >/dev/null 2>&1; then
    groupadd --system tisvlay
fi
if ! getent passwd tisvlay >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin \
        --gid tisvlay --comment "tisvlay daemon" tisvlay
fi

# Create state directory (systemd's StateDirectory= creates it on service
# start, but D-Bus activation may run the daemon before that).
install -d -m 0750 -o tisvlay -g tisvlay /var/lib/tisvlay 2>/dev/null || true

# Reload systemd to pick up new service files
systemctl daemon-reload || true

# Reload D-Bus to pick up the new policy
systemctl reload dbus 2>/dev/null || dbus-send --system --type=method_call \
    --dest=org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus.ReloadConfig 2>/dev/null || true

# Enable (but don't start) the system daemon.
# The user should pair a TV before starting:
#   sudo systemctl start tisvlayd
#   tisvlay pair <address> --vendor lg --name "My TV" -w <mac>
systemctl enable tisvlayd.service 2>/dev/null || true

echo "tisvlay installed. Pair a TV then start the daemon:"
echo "  sudo systemctl start tisvlayd"
echo "  tisvlay pair <address> --vendor lg --name \"My TV\" -w <mac>"
echo "  systemctl --user enable --now tisvlay-session tisvlay-tray"
