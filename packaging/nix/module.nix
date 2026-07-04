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

  # The system daemon's zenoh listener. RuntimeDirectory=tisvlay pins
  # the directory; the daemon chmods the socket 0660 after bind, so
  # tisvlay group membership is the client access gate (see the
  # `users` option).
  systemSocket = "/run/tisvlay/zenoh.sock";
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
      description = "User account under which tisvlay-daemon runs.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "tisvlay";
      description = "Group under which tisvlay-daemon runs.";
    };

    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        User accounts granted control of the daemon. The zenoh socket
        in /run/tisvlay/ is mode 0660 owned by `user`:`group`, so
        socket permission IS the authorization model (replacing the
        old D-Bus policy that let any local user call the daemon).
        Every listed account is added to the daemon group. Desktop
        users running tisvlay-session / tisvlay-tray / the GUI need
        to be listed here.
      '';
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
        Log level for tisvlay-daemon (and its user services), applied
        via the `RUST_LOG` environment variable. Use per-module
        overrides (e.g. `"debug,zenoh=warn"`) by setting `RUST_LOG`
        in a drop-in if you need finer control.
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
        Extra command-line arguments appended to `tisvlay-daemon`'s
        `ExecStart`. Use for one-off overrides the module does not
        expose directly.
      '';
    };

    bindPort = lib.mkOption {
      type = lib.types.nullOr lib.types.port;
      default = 1901;
      example = 1901;
      description = ''
        SSDP source UDP port for TV discovery, pinned so `openFirewall`
        below can drop a tight `--dport` accept rule in place of the
        broad `allow-any-UDP-from-subnet` fallback most distros
        otherwise need.

        Defaults to 1901, matching the daemon's own built-in default,
        so discovery is firewall-targetable out of the box and a user
        who wants the port opened need only set `openFirewall = true`
        — no need to also name the port.

        Plumbed via tisvlay-daemon's `--ssdp-port` CLI flag.

        Set to `0` for an OS-chosen ephemeral port — a firewall rule
        then can't target a single port. Set to `null` to omit the flag
        entirely and defer to the daemon's built-in default; that is
        incompatible with `openFirewall`, since the port isn't known
        at evaluation time.

        See `docs/firewall.md` for the network-level rationale.
      '';
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        When `true`, add `bindPort` to
        `networking.firewall.allowedUDPPorts` so the kernel accepts
        the SSDP responses the daemon's scans elicit. Requires
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
          tv = [
            {
              name = "Living Room";
              vendor = "lg";
              config = {
                ip = "192.168.1.50";
              };
            }
          ];
        }
      '';
      description = ''
        Declarative boot config for tisvlay-daemon, rendered to a TOML
        file in the Nix store and passed via `--config`. Its `[[tv]]`
        entries seed extra TVs at start; entries already present in
        the daemon's writable state file win, and pairing/state
        changes still persist to `/var/lib/tisvlay/state.toml`
        regardless (the boot config is additive, not a lockdown).

        Changes to it only take effect after `nixos-rebuild switch`.
        Leave unset (`null`, the default) to manage TVs entirely at
        runtime via pairing.
      '';
    };

    greeter.enable = lib.mkOption {
      type = lib.types.bool;
      default = config.services.displayManager.plasma-login-manager.enable;
      defaultText = lib.literalExpression "config.services.displayManager.plasma-login-manager.enable";
      description = ''
        Whether to enable the tisvlay greeter idle client. Runs
        `tisvlay-greeter` as the plasma-login-manager greeter user inside
        the greeter Wayland session so managed TVs blank on greeter idle
        and restore on resume — OLED burn-in protection at an unattended
        login screen.

        Defaults to on when `services.displayManager.plasma-login-manager`
        is the active display manager — the only greeter this client
        currently supports (the unit attaches to its greeter target);
        other greeters (greetd + wlroots, GDM) need separate adapters, see
        `docs/ideas/greeter-integration-generalization.md`. Set it
        explicitly to override.

        The feature also needs a non-zero `idle_timeout_seconds` on a
        system profile — set it in the GUI Profiles pane or declaratively;
        it defaults to 0 (off), so enabling this option alone installs the
        client but does not yet blank anything.
      '';
    };

    greeter.user = lib.mkOption {
      type = lib.types.str;
      default = "plasmalogin";
      description = ''
        Account the display manager runs its greeter session as. Added
        to the daemon group when `greeter.enable` is set, so the idle
        client can reach the zenoh socket.
      '';
    };

    cec.enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Enable HDMI-CEC TV control. The daemon opens the kernel CEC
        character devices (`/dev/cecN`), so this relaxes the daemon's
        `PrivateDevices` sandbox to expose them — narrowed straight back
        down with `DeviceAllow=char-cec rw` so only the CEC char subsystem
        is reachable at the cgroup level — and installs a udev rule
        granting the tisvlay group read/write on `/dev/cec*`.

        Off by default: it slightly loosens the daemon sandbox, so only
        enable it on hosts that actually drive a TV over CEC (a CEC-capable
        HDMI or USB adapter that exposes a `/dev/cecN` node — most GPU HDMI
        ports do not). The CEC backend is always compiled in; this option
        only governs device access.
      '';
    };

    cec.pulse8.enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Bring up a Pulse-Eight-class USB-CEC adapter so `/dev/cecN`
        appears — needed because nixpkgs' kernel doesn't build the
        `USB_PULSE8_CEC` driver (native DisplayPort-AUX CEC on amdgpu/i915
        works without this; a USB serial adapter does not).

        Rather than force a full kernel rebuild to flip that one config
        symbol, this builds the in-tree `pulse8-cec` driver as an
        out-of-tree module against the running kernel and loads it, and
        installs the udev rule + `inputattach` systemd unit that binds the
        adapter's serial line discipline (per the kernel's
        `admin-guide/media/cec.rst`). Implies the CEC device-access rules
        of `cec.enable`.

        Only for hosts with a Pulse-Eight USB-CEC adapter (USB id
        `2548:1001` / `2548:1002`).
      '';
    };
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

    # tisvlay-greeter only supports plasma-login-manager today (its units
    # below attach to that greeter's session target). greeter.enable
    # auto-defaults to plasma-login being active; warn — rather than assert —
    # when it is enabled on another DM, so an explicit override degrades
    # visibly instead of silently producing units that never start. See
    # docs/ideas/greeter-integration-generalization.md.
    warnings =
      lib.optional (cfg.greeter.enable && !config.services.displayManager.plasma-login-manager.enable)
        ''
          services.tisvlay.greeter.enable is set, but plasma-login-manager is not
          the active display manager. The greeter idle client only supports
          plasma-login-manager today, so tisvlay-greeter.service will not start.
        '';

    # Dedicated system user and group for tisvlay-daemon. uid/gid stay
    # unset by default — NixOS allocates from the system range and
    # pins the assignment via /var/lib/nixos/uid-map.
    users.groups.${cfg.group} = lib.optionalAttrs (cfg.gid != null) {
      gid = cfg.gid;
    };
    users.users = lib.mkMerge (
      [
        {
          ${cfg.user} = {
            description = "tisvlay daemon user";
            group = cfg.group;
            isSystemUser = true;
          }
          // lib.optionalAttrs (cfg.uid != null) {
            uid = cfg.uid;
          };
        }
      ]
      # Socket-permission grants: listed accounts join the daemon
      # group so they can connect to the 0660 zenoh socket. The
      # greeter user gets the same grant when its client is enabled.
      ++ map (name: {
        ${name}.extraGroups = [ cfg.group ];
      }) (cfg.users ++ lib.optional cfg.greeter.enable cfg.greeter.user)
    );

    # CEC device access + optional Pulse-Eight adapter bring-up.
    #
    # The `/dev/cecN` group grant applies whenever CEC is in use — direct
    # access (`cec.enable`) or the pulse8 adapter (`cec.pulse8.enable`).
    # The pulse8 rule triggers `inputattach` to bind the adapter's serio
    # line discipline so `/dev/cecN` appears (kernel
    # `admin-guide/media/cec.rst`).
    services.udev.extraRules = lib.mkMerge [
      (lib.mkIf (cfg.cec.enable || cfg.cec.pulse8.enable) ''
        KERNEL=="cec[0-9]*", GROUP="${cfg.group}", MODE="0660"
      '')
      (lib.mkIf cfg.cec.pulse8.enable ''
        ACTION=="add", SUBSYSTEM=="tty", ATTRS{idVendor}=="2548", ATTRS{idProduct}=="1001", TAG+="systemd", ENV{SYSTEMD_WANTS}+="pulse8-cec-inputattach@%k.service"
        ACTION=="add", SUBSYSTEM=="tty", ATTRS{idVendor}=="2548", ATTRS{idProduct}=="1002", TAG+="systemd", ENV{SYSTEMD_WANTS}+="pulse8-cec-inputattach@%k.service"
      '')
    ];

    # Build the in-tree pulse8-cec driver out-of-tree against the running
    # kernel (nixpkgs doesn't ship USB_PULSE8_CEC) and load it.
    boot.extraModulePackages = lib.mkIf cfg.cec.pulse8.enable [
      (config.boot.kernelPackages.callPackage ./pulse8-cec.nix { })
    ];
    boot.kernelModules = lib.mkIf cfg.cec.pulse8.enable [ "pulse8_cec" ];

    # inputattach holds the serio line discipline that binds pulse8-cec; it
    # must stay in the foreground (no --daemon) or /dev/cecN disappears.
    systemd.services."pulse8-cec-inputattach@" = lib.mkIf cfg.cec.pulse8.enable {
      description = "inputattach for pulse8-cec device on %I";
      serviceConfig = {
        Type = "simple";
        ExecStart = "${pkgs.linuxConsoleTools}/bin/inputattach --pulse8-cec /dev/%I";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };

    # Make the CLI available system-wide. The GUI .desktop entry
    # and D-Bus session activation file ship with the package
    # (see package.nix): the .desktop via makeDesktopItem +
    # copyDesktopItems (the 622-package nixpkgs idiom), the
    # .service via postInstall + substituteInPlace.
    environment.systemPackages = [ cfg.package ];

    # Firewall: gated by both openFirewall (opt-in) and bindPort
    # being set (assertion above already enforces this combination).
    # Opens just the one UDP port the daemon is configured to bind —
    # narrower than the per-subnet UDP allow-list distros otherwise
    # need for SSDP responses. See docs/firewall.md.
    networking.firewall.allowedUDPPorts = lib.optionals (cfg.openFirewall && cfg.bindPort != null) [
      cfg.bindPort
    ];

    # System daemon: owns the TV fleet and the zenoh socket clients
    # connect to. State lives in /var/lib/tisvlay/state.toml
    # (StateDirectory), writable by the daemon so pairing can persist
    # credentials. Mode 0750 — only tisvlay user/group can read.
    # Clients talk to the daemon over the socket instead of reading
    # the file.
    systemd.services.tisvlay-daemon = {
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
          [
            (lib.getExe' cfg.package "tisvlay-daemon")
            "--socket"
            systemSocket
            "--state"
            "/var/lib/tisvlay/state.toml"
          ]
          ++ lib.optionals (configFile != null) [
            "--config"
            "${configFile}"
          ]
          ++ lib.optionals (cfg.bindPort != null) [
            "--ssdp-port"
            (toString cfg.bindPort)
          ]
          ++ cfg.extraArgs
        );
        User = cfg.user;
        Group = cfg.group;
        StateDirectory = "tisvlay";
        StateDirectoryMode = "0750";
        # Socket dir: group-traversable for tisvlay-group clients,
        # preserved across restarts so connect-only clients redial
        # the same path.
        RuntimeDirectory = "tisvlay";
        RuntimeDirectoryMode = "0750";
        RuntimeDirectoryPreserve = "restart";
        Restart = "on-failure";
        RestartSec = 5;

        # --- Security hardening -----------------------------------------
        # Baseline filesystem / privilege confinement.
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        # PrivateDevices gives a minimal private /dev with no /dev/cecN, so
        # the CEC backend can't open its character device under it. Relaxed
        # only when cec.enable is set; the DeviceAllow merge below then
        # re-narrows access to the cec char subsystem.
        PrivateDevices = !(cfg.cec.enable || cfg.cec.pulse8.enable);
        NoNewPrivileges = true;

        # Network: UNIX (zenoh socket), INET{4,6} (WebSocket to TVs, WoL,
        # SSDP), NETLINK (carrier / route monitoring for NIC failover).
        # No PACKET/BLUETOOTH.
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

        # Files created by the daemon (pairing credentials, state
        # rewrites) should not be world-readable. `StateDirectoryMode`
        # already enforces 0750 on the directory; UMask takes care of
        # the files within. The zenoh socket is the one exception —
        # the daemon chmods it 0660 after bind.
        UMask = "0077";

        # Clean up any SysV IPC the daemon (or its future children)
        # might leak at shutdown.
        RemoveIPC = true;
      }
      // lib.optionalAttrs (cfg.cec.enable || cfg.cec.pulse8.enable) {
        # With PrivateDevices relaxed (above) so /dev/cecN is visible,
        # scope the cgroup device policy to the cec char subsystem so the
        # daemon can reach CEC adapters and nothing else new. The udev rule
        # in the config block grants the tisvlay group rw on the nodes.
        DeviceAllow = [ "char-cec rw" ];
      };
    };

    # User session monitor: reacts to session lock/unlock and idle
    # events, and serves user profiles on the per-user socket in
    # $XDG_RUNTIME_DIR. The system socket is passed explicitly — the
    # packaged daemon listens in /run/tisvlay, not the per-user
    # runtime dir the dev default points at.
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
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' cfg.package "tisvlay-session")
          "--system-socket"
          systemSocket
        ];
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # System tray icon: per-user TV control menu over a live state
    # mirror; user profiles appear via the session service's socket.
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
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' cfg.package "tisvlay-tray")
          "--socket"
          systemSocket
        ];
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # Greeter idle client. Runs as the plasma-login-manager greeter user
    # inside the greeter Wayland session — a systemd *user* manager started by
    # plasma-login on the session bus — so it MUST be a systemd.user unit: a
    # system service can't be WantedBy= the user-scoped greeter target and
    # wouldn't inherit the compositor's XDG_RUNTIME_DIR/WAYLAND_DISPLAY. The
    # greeter user joins the daemon group below so it can reach the socket.
    systemd.user.services.tisvlay-greeter = lib.mkIf cfg.greeter.enable {
      description = "tisvlay greeter idle client (plasma-login-manager)";
      documentation = [ "https://github.com/ruffsl/tisvlay" ];
      after = [ "plasma-login-kwin_wayland.service" ];
      requires = [ "plasma-login-kwin_wayland.service" ];
      # partOf stops us with the greeter session; the target itself bindsTo the
      # compositor, so a compositor death stops the target and thus us.
      partOf = [ "plasma-login-wayland.target" ];
      # Deliberately NOT wantedBy graphical-session.target (that is every real
      # user session); attached to the greeter target via the drop-in below.
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' cfg.package "tisvlay-greeter")
          "--socket"
          systemSocket
        ];
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
