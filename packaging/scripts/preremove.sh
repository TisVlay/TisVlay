#!/bin/sh
# Pre-removal script for tisvlay.
# Stops and disables services before package removal.

set -e

# Stop and disable system daemon
systemctl stop tisvlayd.service 2>/dev/null || true
systemctl disable tisvlayd.service 2>/dev/null || true

# Reload systemd
systemctl daemon-reload || true

# Note: the tisvlay user/group and /var/lib/tisvlay/ are intentionally
# preserved on removal — they contain TV credentials. The user can
# manually remove them with:
#   sudo userdel tisvlay
#   sudo groupdel tisvlay
#   sudo rm -rf /var/lib/tisvlay
