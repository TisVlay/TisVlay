#!/bin/sh
# Pre-removal script for tisvlay.
# Stops and disables services on real removal — but never on an upgrade.

set -e

# Tear down only on an actual removal, never on an upgrade/reinstall
# (otherwise the old package's teardown runs after the new package's
# postinstall and leaves the daemon stopped + units disabled). The three
# packagers sharing this one script signal removal-vs-upgrade in $1
# differently, so exclude the upgrade signals rather than allow-list
# removals:
#   rpm    %preun      — count: 0 = removal, >=1 = upgrade/reinstall
#   deb    prerm       — word: remove = removal; upgrade/failed-upgrade/
#                        deconfigure = not a removal
#   pacman pre_remove  — the version string, and pacman runs this hook
#                        ONLY on removal (upgrades use pre_upgrade), so a
#                        version string must fall through to teardown
case "$1" in
1 | upgrade | failed-upgrade | deconfigure)
    # Upgrade in flight: leave the running stack alone. The new package's
    # postinstall reconciles enablement and restarts the daemon.
    ;;
*)
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
    ;;
esac

# Note: the tisvlay user/group and /var/lib/tisvlay/ are intentionally
# preserved on removal — they contain TV credentials. The user can
# manually remove them with:
#   sudo userdel tisvlay
#   sudo groupdel tisvlay
#   sudo rm -rf /var/lib/tisvlay
