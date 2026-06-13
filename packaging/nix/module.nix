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

    bindPort = lib.mkOption {
      type = lib.types.nullOr lib.types.port;
      default = null;
      example = 1901;
      description = ''
        Pin tisvlayd's SSDP source UDP port for firewall-friendly TV
        discovery — pairs with `openFirewall` below to drop a tight
        `--dport` accept rule in place of the
        broad `allow-any-UDP-from-subnet` fallback most distros
        otherwise need. When unset (`null`, the default), the
        kernel hands out an ephemeral port per scan and a firewall
        rule needs to be correspondingly wide.

        Plumbed via tisvlayd's `--bind-port` CLI flag, which
        overrides the `[discovery].bind_port` setting in the
        daemon's writable config file. Using the CLI flag (rather
        than baking the value into `settings`) keeps the config
        file writable so the pairing wizard can still persist
        credentials.

        See `docs/firewall.md` for the network-level rationale.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        When `true`, add `bindPort` to
        `networking.firewall.allowedUDPPorts` so the kernel accepts
        the SSDP responses tisvlayd's scans elicit. Requires
        `bindPort` to be set — opening a known port is the whole
        point.

        Defaults to `false`. Opening a port is a deliberate
        decision and the module shouldn't widen the host's attack
        surface unconditionally.
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

    greeter.enable = lib.mkEnableOption ''
      the tisvlay greeter idle client. Runs `tisvlay-greeter` as the
      plasma-login-manager greeter user inside the greeter Wayland session
      so managed TVs blank on greeter idle and restore on resume — OLED
      burn-in protection at an unattended login screen.

      Only meaningful when the display manager is plasma-login-manager (the
      unit attaches to its greeter target). The feature also needs a
      non-zero `idle_timeout_seconds` on a system profile — set it in the
      GUI Profiles pane or declaratively; it defaults to 0 (off), so
      enabling this option alone installs the client but does not yet blank
      anything
    '';
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.openFirewall || cfg.bindPort != null;
        message = ''
          services.tisvlay.openFirewall = true requires
          services.tisvlay.bindPort to be set — there's no port to
          open otherwise. Either set bindPort to a specific UDP port
          (e.g. 1901) or leave openFirewall = false and configure
          the firewall manually.
        '';
      }
    ];

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

    # Make the CLI available system-wide. The GUI .desktop entry
    # and D-Bus session activation file ship with the package
    # (see package.nix): the .desktop via makeDesktopItem +
    # copyDesktopItems (the 622-package nixpkgs idiom), the
    # .service via postInstall + substituteInPlace.
    environment.systemPackages = [ cfg.package ];

    # Firewall: gated by both openFirewall (opt-in) and bindPort
    # being set (assertion above already enforces this combination).
    # Opens just the one UDP port tisvlayd is configured to bind —
    # narrower than the per-subnet UDP allow-list distros otherwise
    # need for SSDP responses. See docs/firewall.md.
    networking.firewall.allowedUDPPorts = lib.optionals (cfg.openFirewall && cfg.bindPort != null) [
      cfg.bindPort
    ];

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
          ++ lib.optionals (cfg.bindPort != null) [
            "--bind-port"
            (toString cfg.bindPort)
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
        PrivateDevices = true;
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
      };
    };

    # User session monitor: reacts to session lock/unlock and idle events.
    # Loads user config from ~/.config/tisvlay/session.toml (optional).
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

    # Greeter idle client. Runs as the plasma-login-manager greeter user
    # inside the greeter Wayland session — a systemd *user* manager started by
    # plasma-login on the session bus — so it MUST be a systemd.user unit: a
    # system service can't be WantedBy= the user-scoped greeter target and
    # wouldn't inherit the compositor's XDG_RUNTIME_DIR/WAYLAND_DISPLAY. The
    # daemon's existing context="default" D-Bus policy already lets the greeter
    # user call org.tisvlay.Daemon, so no extra policy is needed.
    systemd.user.services.tisvlay-greeter = lib.mkIf cfg.greeter.enable {
      description = "tisvlay greeter idle client (plasma-login-manager)";
      documentation = [ "https://github.com/ruffsl/tisvlay" ];
      after = [ "plasma-login-kwin_wayland.service" ];
      requires = [ "plasma-login-kwin_wayland.service" ];
      partOf = [ "plasma-login-wayland.target" ];
      bindsTo = [ "plasma-login-wayland.target" ];
      # Deliberately NOT wantedBy graphical-session.target (that is every real
      # user session); attached to the greeter target via the drop-in below.
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        ExecStart = lib.getExe' cfg.package "tisvlay-greeter";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # Pull the client into the greeter target only. The target is a
    # vendor-shipped user unit; `asDropin` adds a Wants= without clobbering it.
    systemd.user.targets."plasma-login-wayland" = lib.mkIf cfg.greeter.enable {
      overrideStrategy = "asDropin";
      wants = [ "tisvlay-greeter.service" ];
    };
  };

  meta.maintainers = [
    {
      name = "ruffsl";
      github = "ruffsl";
    }
  ];
}
