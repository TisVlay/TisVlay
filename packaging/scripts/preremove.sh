#!/bin/sh
# Pre-removal script for tisvlay.
# Stops and disables services before package removal.

set -e

# Stop and disable system daemon
systemctl stop tisvlayd.service 2>/dev/null || true
systemctl disable tisvlayd.service 2>/dev/null || true

# Note: user services (tisvlay-session, tisvlay-tray) are per-user
# and managed by individual users. We don't touch them here — systemd
# will handle missing binaries on the next user login.

# Reload systemd
systemctl daemon-reload || true
