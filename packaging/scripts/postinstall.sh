#!/bin/sh
# Post-installation script for tisvlay.
# Creates config directory, reloads systemd and D-Bus.

set -e

# Create config directory if it doesn't exist
mkdir -p /etc/tisvlay
chmod 750 /etc/tisvlay

# Reload systemd to pick up new service files
systemctl daemon-reload || true

# Reload D-Bus to pick up the new policy
systemctl reload dbus 2>/dev/null || dbus-send --system --type=method_call \
    --dest=org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus.ReloadConfig 2>/dev/null || true

# Enable (but don't start) the system daemon.
# The user should configure /etc/tisvlay/config.toml before starting.
systemctl enable tisvlayd.service 2>/dev/null || true

echo "tisvlay installed. Configure /etc/tisvlay/config.toml then run:"
echo "  sudo systemctl start tisvlayd"
echo "  systemctl --user enable --now tisvlay-session tisvlay-tray"
