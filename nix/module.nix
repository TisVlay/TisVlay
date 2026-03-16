{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.tisvlay;

  dbusPolicy = ''
    <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
     "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
    <busconfig>
      <!-- Allow the tisvlay system user (or root) to own the service -->
      <policy user="tisvlay">
        <allow own="org.tisvlay.Daemon"/>
      </policy>
      <policy user="root">
        <allow own="org.tisvlay.Daemon"/>
      </policy>

      <!-- Allow any user to call methods and receive signals -->
      <policy context="default">
        <allow send_destination="org.tisvlay.Daemon"/>
        <allow receive_sender="org.tisvlay.Daemon"/>
      </policy>
    </busconfig>
  '';
in
{
  options.services.tisvlay = {
    enable = lib.mkEnableOption "tisvlay TV control daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.tisvlay";
      description = "The tisvlay package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Dedicated system user and group for tisvlayd.
    users.groups.tisvlay = { };
    users.users.tisvlay = {
      description = "tisvlay daemon user";
      group = "tisvlay";
      isSystemUser = true;
    };

    # D-Bus policy: allow the tisvlay user to own the service name.
    environment.etc."dbus-1/system.d/org.tisvlay.Daemon.conf".text = dbusPolicy;

    # Make the CLI available system-wide.
    environment.systemPackages = [ cfg.package ];

    # System daemon: manages TV connections, exposes D-Bus API.
    # Config lives in /var/lib/tisvlay/config.toml (StateDirectory), writable
    # by the daemon so the pairing wizard can persist credentials.
    systemd.services.tisvlayd = {
      description = "tisvlay TV control daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "tisvlayd";
        ExecReload = "${pkgs.util-linux}/bin/kill -HUP $MAINPID";
        User = "tisvlay";
        Group = "tisvlay";
        # /var/lib/tisvlay/ — writable, mode 0755 so user services can read the config.
        StateDirectory = "tisvlay";
        StateDirectoryMode = "0755";
        Restart = "on-failure";
        RestartSec = 5;
        # Security hardening
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };

    # User session monitor: reacts to screen lock/unlock and suspend/resume.
    # Reads shared config from /var/lib/tisvlay/config.toml for TV profiles.
    systemd.user.services.tisvlay-session = {
      description = "tisvlay session event monitor";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "tisvlay-session"} --config /var/lib/tisvlay/config.toml";
        ExecReload = "${pkgs.util-linux}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # System tray icon: per-user TV control menu via D-Bus.
    systemd.user.services.tisvlay-tray = {
      description = "tisvlay system tray";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "tisvlay-tray";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
