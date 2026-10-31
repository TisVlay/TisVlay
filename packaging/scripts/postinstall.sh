#!/bin/sh
# Post-installation script for tisvlay.
# Creates system user and reloads systemd.

set -e

# Create dedicated system user and group (if they don't exist).
# The daemon runs as this user; state in /var/lib/tisvlay/ is owned
# by it via systemd StateDirectory. The group is the durable access
# gate: the daemon's zenoh socket in /run/tisvlay/ is 0660
# tisvlay:tisvlay, and accounts that need TV control from SSH/remote/
# headless join it (`tisvlay enable-access` is the one-click grant).
# The console user needs no membership — the daemon grants the active
# seat's graphical user a socket ACL (--seat-access, default on) — and
# the login greeter reaches the socket via the --greeter-access group
# ACL, since the modern GDM greeter is a dynamic-userdb account
# /etc/group can't reach.
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

# Enable the system daemon, and on upgrade restart it onto the new
# binary: the old package's preremove skips its stop on upgrade, so a
# running daemon would otherwise keep the old code — while try-restart's
# no-op on an inactive unit is what respects an admin's deliberate stop
# across upgrades. Guarded so image builds without a live systemd stay
# quiet.
systemctl enable tisvlay-daemon.service 2>/dev/null || true
# The wakeup unit runs with each sleep (WantedBy=sleep.target); with no
# CEC adapter paired, its list is empty and it changes nothing.
systemctl enable tisvlay-wakeup.service 2>/dev/null || true
systemctl try-restart tisvlay-daemon.service 2>/dev/null || true

# Start only on FIRST install — an upgrade must never override an
# admin's runtime stop, and a printed "now start it" hint reaches no one
# under an app-store install. Fresh install is $1=configure with no old
# version in $2 (deb), $1=1 (rpm), or version-valued $1 with $2 empty
# (pacman). RPM upgrade counts are numeric; pacman upgrades set $2 to the
# old version.
# Debian goes through deb-systemd-invoke when present so policy-rc.d is
# honored in chroots and image builds.
if [ "$1" = "configure" ] && [ -z "$2" ]; then
    if command -v deb-systemd-invoke >/dev/null 2>&1; then
        deb-systemd-invoke start tisvlay-daemon.service >/dev/null 2>&1 || true
    else
        systemctl start tisvlay-daemon.service 2>/dev/null || true
    fi
elif [ "$1" = "1" ]; then
    systemctl start tisvlay-daemon.service 2>/dev/null || true
elif [ -z "$2" ]; then
    case "$1" in
        *[!0-9]*) systemctl start tisvlay-daemon.service 2>/dev/null || true ;;
    esac
fi

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
# greeter-group ACL (`tisvlay service daemon --greeter-access`) — no group edit here,
# since the modern GDM greeter is a dynamic-userdb account /etc/group can't
# reach.
systemctl --global enable tisvlay-greeter-gdm.service 2>/dev/null || true

echo "tisvlay installed; the daemon is enabled and starts on fresh installs"
echo "(upgrades keep its running state)."
echo "The user at the console has TV control automatically; grant an account"
echo "durable access (SSH, remote, headless) with: tisvlay enable-access"
echo "The session + tray start automatically on your next graphical login;"
echo "start them now without re-login with:"
echo "  systemctl --user start tisvlay-session tisvlay-tray"
echo "Pair a TV:  tisvlay pair \"My TV\" --vendor lg --params '{\"ip\":\"<address>\"}'"
