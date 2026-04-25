{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.tisvlay;

  tomlFormat = pkgs.formats.toml { };
  configFile =
    if cfg.settings == null then null else tomlFormat.generate "tisvlay-config.toml" cfg.settings;

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
      default = pkgs.tisvlay or (pkgs.callPackage ./package.nix { });
      defaultText = lib.literalExpression "pkgs.tisvlay";
      description = ''
        The tisvlay package to use. Defaults to `pkgs.tisvlay` when the
        flake's `overlays.default` is applied; otherwise falls back to
        building from this module's sibling `package.nix`.
      '';
    };

    enableKernelModule = lib.mkOption {
      type = lib.types.bool;
      default = config.boot.kernelPackages.kernel.features.rust or false;
      defaultText = lib.literalExpression "config.boot.kernelPackages.kernel.features.rust or false";
      description = ''
        Build and load the `tisvlay_backlight` out-of-tree kernel
        module, which provides per-TV `/sys/class/backlight/tisvlay-*`
        devices for native KDE brightness sync
        (`sync_backlight = "kernel"`).

        Requires a kernel built with `CONFIG_RUST=y`. Auto-enables
        when the running kernel's `features.rust` is true; override
        when you want to opt out or when detection misses.
      '';
    };

    kernelModulePackage = lib.mkOption {
      type = lib.types.package;
      default = config.boot.kernelPackages.callPackage ../../module/tisvlay-backlight { };
      defaultText = lib.literalExpression "config.boot.kernelPackages.callPackage ../../module/tisvlay-backlight { }";
      description = ''
        The `tisvlay_backlight` kernel module derivation, built against
        the active `boot.kernelPackages`. Override to inject a
        pre-built package (e.g. from a flake output).
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
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = ''
        Optional fixed system UID for the tisvlay user. Leave `null`
        (the default) to let NixOS allocate a system UID dynamically
        and persist it via `/var/lib/nixos/uid-map` — this keeps the
        UID stable across rebuilds on a given host.

        Set a specific number if you need the UID to match across
        multiple hosts (e.g. shared state over network filesystems).
        If this module is ever upstreamed to nixpkgs, a fixed UID
        will be reserved in `nixos/modules/misc/ids.nix`; until then,
        out-of-tree assignments risk colliding with other services.
      '';
    };

    gid = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = ''
        Optional fixed system GID for the tisvlay group. Same caveats
        as `uid`.
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
      example = [
        "--config"
        "/etc/tisvlay/custom.toml"
      ];
      description = ''
        Extra command-line arguments appended to `tisvlayd`'s
        `ExecStart`. Use for one-off overrides the module does not
        expose directly.
      '';
    };

    settings = lib.mkOption {
      type = lib.types.nullOr tomlFormat.type;
      default = null;
      example = lib.literalExpression ''
        {
          profile.system = {
            shutdown = "power_off";
            sleep = "screen_off";
          };
          tv = [
            {
              id = "living_room";
              name = "Living Room";
              vendor = "lg";
              managed = true;
            }
          ];
        }
      '';
      description = ''
        Declarative system configuration for tisvlayd, rendered to a
        TOML file in the Nix store and passed via `--config`.

        When set (non-null), the config file is read-only (Nix store
        path). This means:

        - Pairing TVs via `tisvlay pair` or the D-Bus `AddTv` method
          will fail to persist (the daemon cannot write to the file).
        - Changes only take effect after `nixos-rebuild switch`.

        Leave unset (`null`, the default) to preserve the normal
        behavior: tisvlayd reads and writes
        `/var/lib/tisvlay/config.toml` (via `StateDirectory`), which
        the pairing wizard can modify.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Dedicated system user and group for tisvlayd. uid/gid stay
    # unset by default — NixOS allocates from the system range and
    # pins the assignment via /var/lib/nixos/uid-map.
    users.groups.${cfg.group} = lib.optionalAttrs (cfg.gid != null) {
      gid = cfg.gid;
    };
    users.users.${cfg.user} = {
      description = "tisvlay daemon user";
      group = cfg.group;
      isSystemUser = true;
    }
    // lib.optionalAttrs (cfg.uid != null) {
      uid = cfg.uid;
    };

    # D-Bus policy: allow the tisvlay user to own the service name.
    # Uses services.dbus.packages (the idiomatic NixOS approach) rather than
    # environment.etc, since NixOS manages /etc/dbus-1/system.d/ via the dbus module.
    services.dbus.packages = [
      (pkgs.writeTextDir "share/dbus-1/system.d/org.tisvlay.Daemon.conf" dbusPolicy)
    ];

    # Make the CLI available system-wide.
    environment.systemPackages = [ cfg.package ];

    # Kernel module for virtual /sys/class/backlight/tisvlay-* devices.
    # Loaded at boot so /dev/tisvlay-bl is present before tisvlayd
    # starts — otherwise the daemon's chardev driver quietly stays
    # disabled and brightness sync falls back to the session path.
    boot.extraModulePackages = lib.mkIf cfg.enableKernelModule [
      cfg.kernelModulePackage
    ];
    boot.kernelModules = lib.mkIf cfg.enableKernelModule [ "tisvlay_bl" ];

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
          [ (lib.getExe' cfg.package "tisvlayd") ]
          ++ lib.optionals (configFile != null) [
            "--config"
            "${configFile}"
          ]
          ++ cfg.extraArgs
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

        # --- Security hardening -----------------------------------------
        # Baseline filesystem / privilege confinement.
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        # Hide all physical devices by default; relaxed below when the
        # kernel-module brightness sync is enabled so the daemon can
        # open /dev/tisvlay-bl.
        PrivateDevices = !cfg.enableKernelModule;
        NoNewPrivileges = true;

        # Network: UNIX (D-Bus), INET{4,6} (WebSocket to TVs, WoL), NETLINK
        # (carrier / route monitoring for NIC failover). No PACKET/BLUETOOTH.
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
        ];

        # No child processes, no mmap+x, no realtime scheduling, no setuid.
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        RestrictNamespaces = true;

        # Kernel surface: deny /proc/sys writes, module load, kmsg reads,
        # cgroup edits, and hostname/clock mutation.
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectClock = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";

        # Daemon needs no capabilities — runs as an unprivileged system user.
        CapabilityBoundingSet = [ "" ];
        AmbientCapabilities = [ "" ];

        # System-call allowlist: block @privileged/@obsolete/etc. The
        # @system-service set is designed for long-running daemons.
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];

        # Files created by the daemon (pairing credentials, config
        # rewrites) should not be world-readable. `StateDirectoryMode`
        # already enforces 0750 on the directory; UMask takes care of
        # the files within.
        UMask = "0077";

        # Clean up any SysV IPC the daemon (or its future children)
        # might leak at shutdown.
        RemoveIPC = true;
      }
      // lib.optionalAttrs cfg.enableKernelModule {
        # With PrivateDevices disabled above, restrict which devices
        # the daemon can actually open to just the virtual-backlight
        # chardev — the cgroup device controller enforces this
        # regardless of /dev visibility.
        DeviceAllow = [ "/dev/tisvlay-bl rw" ];
        DevicePolicy = "closed";
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
      after = [
        "graphical-session.target"
        "tisvlay-session.service"
      ];
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
