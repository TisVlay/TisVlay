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
      <!-- Allow the configured tisvlay user (or root) to own the service -->
      <policy user="${cfg.user}">
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
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = ''
        The tisvlay package to use. Defaults to building from this
        module's sibling `package.nix`. Override when the module is
        imported via a flake that exposes the package elsewhere.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "tisvlay";
      description = "User account under which tisvlayd runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "tisvlay";
      description = "Group under which tisvlayd runs.";
    };

    uid = lib.mkOption {
      type = lib.types.int;
      default = 979;
      description = ''
        Stable system UID for the tisvlay user. Fixed to keep
        `StateDirectory` ownership deterministic across rebuilds.
        Override if 979 collides with another service on your system.
        If this module is ever upstreamed to nixpkgs, the UID will be
        reassigned via `nixos/modules/misc/ids.nix`.
      '';
    };

    gid = lib.mkOption {
      type = lib.types.int;
      default = 979;
      description = ''
        Stable system GID for the tisvlay group. Same caveats as `uid`.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.enum [
        "trace"
        "debug"
        "info"
        "warn"
        "error"
      ];
      default = "info";
      description = ''
        Log level for tisvlayd (and its user services), applied via the
        `RUST_LOG` environment variable. Use per-module overrides (e.g.
        `"debug,zbus=warn"`) through `extraArgs` or by setting
        `RUST_LOG` in a drop-in if you need finer control.
      '';
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "--config" "/etc/tisvlay/custom.toml" ];
      description = ''
        Extra command-line arguments appended to `tisvlayd`'s
        `ExecStart`. Use for one-off overrides the module does not
        expose directly.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Dedicated system user and group for tisvlayd.
    users.groups.${cfg.group} = {
      gid = cfg.gid;
    };
    users.users.${cfg.user} = {
      description = "tisvlay daemon user";
      group = cfg.group;
      uid = cfg.uid;
      isSystemUser = true;
    };

    # D-Bus policy: allow the tisvlay user to own the service name.
    # Uses services.dbus.packages (the idiomatic NixOS approach) rather than
    # environment.etc, since NixOS manages /etc/dbus-1/system.d/ via the dbus module.
    services.dbus.packages = [
      (pkgs.writeTextDir "share/dbus-1/system.d/org.tisvlay.Daemon.conf" dbusPolicy)
    ];

    # Make the CLI available system-wide.
    environment.systemPackages = [ cfg.package ];

    # System daemon: manages TV connections, exposes D-Bus API.
    # Config lives in /var/lib/tisvlay/config.toml (StateDirectory), writable
    # by the daemon so the pairing wizard can persist credentials.
    # Mode 0750 — only tisvlay user/group can read (contains TV credentials).
    # Session services query the daemon via D-Bus instead of reading the file.
    systemd.services.tisvlayd = {
      description = "tisvlay TV control daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      documentation = [ "https://github.com/ruffsl/tisvlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        ExecStart = lib.escapeShellArgs (
          [ (lib.getExe' cfg.package "tisvlayd") ] ++ cfg.extraArgs
        );
        ExecReload = "${pkgs.util-linux}/bin/kill -HUP $MAINPID";
        User = cfg.user;
        Group = cfg.group;
        StateDirectory = "tisvlay";
        StateDirectoryMode = "0750";
        RuntimeDirectory = "tisvlay";
        RuntimeDirectoryPreserve = "restart";
        Restart = "on-failure";
        RestartSec = 5;
        # Security hardening
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };

    # User session monitor: reacts to session lock/unlock and idle events.
    # Loads user config from ~/.config/tisvlay/config.toml (optional).
    # Queries the daemon via D-Bus for TV list and system default profile.
    systemd.user.services.tisvlay-session = {
      description = "tisvlay session event monitor";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      documentation = [ "https://github.com/ruffsl/tisvlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "tisvlay-session";
        ExecReload = "${pkgs.util-linux}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # System tray icon: per-user TV control menu via D-Bus.
    # Communicates with daemon for TV control and session service for profiles.
    systemd.user.services.tisvlay-tray = {
      description = "tisvlay system tray";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" "tisvlay-session.service" ];
      partOf = [ "graphical-session.target" ];
      documentation = [ "https://github.com/ruffsl/tisvlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "tisvlay-tray";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };

  meta.maintainers = [
    {
      name = "ruffsl";
      github = "ruffsl";
    }
  ];
}
