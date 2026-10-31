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

  # Which greeter the idle client attaches to — a system runs one DM.
  # Both expose a greeter-only session target the client hangs off of.
  plasmaLoginGreeter = config.services.displayManager.plasma-login-manager.enable;
  gdmGreeter = config.services.displayManager.gdm.enable;
  # The greeter's primary group, which the daemon grants a socket ACL via
  # --greeter-group so the idle client can reach the zenoh socket. The
  # modern GDM greeter is a dynamic-userdb account /etc/group can't reach,
  # so keying on the group is the portable grant; nixpkgs puts its static
  # `gdm-greeter{,-2..5}` accounts in group `gdm` and the plasma-login
  # greeter in group `plasmalogin`. A system runs one DM, so unambiguous.
  greeterGroup =
    if gdmGreeter then
      "gdm"
    else if plasmaLoginGreeter then
      "plasmalogin"
    else
      null;
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
        building from this module's sibling `package.nix`. Either way it
        builds against the host's nixpkgs; set this to the flake's
        `packages.''${system}.default` to use the release builds in the
        tisvlay.cachix.org binary cache instead.
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
        "--seed"
        "/etc/tisvlay/seed.toml"
      ];
      description = ''
        Extra command-line arguments appended to `tisvlay service daemon`'s
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

        Plumbed via `tisvlay service daemon --ssdp-port`.

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
        Declarative seed config for tisvlay-daemon, rendered to a
        TOML file in the Nix store and passed via `--seed`.
        Provisioning semantics: re-applied every start; entries whose
        id already exists in the daemon-owned
        `/var/lib/tisvlay/daemon.toml` are skipped, so runtime edits
        stick and pairing still persists there. A still-declared TV
        removed at runtime returns on the next start — drop the
        entry to drop the TV.

        Changes to it only take effect after `nixos-rebuild switch`.
        Leave unset (`null`, the default) to manage TVs entirely at
        runtime via pairing.
      '';
    };

    greeter.enable = lib.mkOption {
      type = lib.types.bool;
      default = plasmaLoginGreeter || gdmGreeter;
      defaultText = lib.literalExpression "plasma-login-manager or GDM is the active display manager";
      description = ''
        Whether to enable the tisvlay greeter idle client. Runs
        `tisvlay-greeter` as the greeter user inside the greeter Wayland
        session so managed TVs blank on greeter idle and restore on
        resume — OLED burn-in protection at an unattended login screen.

        Defaults to on when `plasma-login-manager` or `gdm` is the active
        display manager — the greeters this client supports. It attaches
        to the greeter's session target: `plasma-login-wayland.target` for
        plasma-login (KWin, ext-idle-notify), or `gnome-session@gnome-login.target`
        for GDM (Mutter, via `org.gnome.Mutter.IdleMonitor`). Other
        greeters (greetd + wlroots) need separate adapters, see
        `docs/ideas/greeter-integration-generalization.md`. Set it
        explicitly to override.

        The feature also needs a non-zero `idle_timeout_seconds` on a
        system profile — set it in the GUI Profiles pane or declaratively;
        it defaults to 0 (off), so enabling this option alone installs the
        client but does not yet blank anything.
      '';
    };

    sunshinePort = lib.mkOption {
      type = lib.types.nullOr lib.types.port;
      default = null;
      example = 48989;
      description = ''
        Sunshine's base port on loopback, where the session service's
        stream detector polls the unauthenticated `/serverinfo`
        to arm TVs while a stream is active. Set it to Sunshine's
        configured `port` when that's not the default — e.g. when
        Sunshine is moved aside so Moonshine keeps the GameStream
        defaults, the default port would answer for Moonshine instead.
        `null` keeps the session service's built-in default, Sunshine's
        own default port.
      '';
    };

    seatAccess = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Grant the active seat's graphical user socket access via a
        daemon-applied POSIX ACL while their session is the seat
        foreground (uaccess-style presence grant). Lets the person at
        the console use TisVlay with no group enrollment; the `users`
        list (the `tisvlay` group) remains the durable grant for SSH,
        remote, and headless use. Disable to restrict the socket to
        the group and greeter only.
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

    # tisvlay-greeter attaches to a plasma-login or GDM greeter session
    # target. greeter.enable auto-defaults to one of those being active;
    # warn — rather than assert — when it is enabled on another DM, so an
    # explicit override degrades visibly instead of silently producing a
    # unit that never starts. See
    # docs/ideas/greeter-integration-generalization.md.
    warnings = lib.optional (cfg.greeter.enable && !plasmaLoginGreeter && !gdmGreeter) ''
      services.tisvlay.greeter.enable is set, but neither plasma-login-manager
      nor gdm is the active display manager. The greeter idle client supports
      only those two, so tisvlay-greeter.service will not start.
    '';

    # Dedicated system user and group for tisvlay-daemon. uid/gid stay
    # unset by default — NixOS allocates from the system range and
    # pins the assignment via /var/lib/nixos/uid-map.
    # The greeter reaches the socket via the daemon's --greeter-group ACL
    # (below), not group membership — so no greeter accounts are added here.
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
      # Socket-permission grants: listed accounts join the daemon group
      # so they can connect to the 0660 zenoh socket.
      ++ map (name: {
        ${name}.extraGroups = [ cfg.group ];
      }) cfg.users
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
    # A resume from suspend resets the adapter's USB link and hangs up the
    # tty under it, and inputattach then exits *cleanly*, so on-failure
    # would leave /dev/cecN gone until someone restarts the unit: restart
    # it whatever the exit. Bound to the tty, so a real unplug stops it
    # instead of restart-looping on a missing node; the udev rule above
    # starts it again when the adapter returns.
    systemd.services."pulse8-cec-inputattach@" = lib.mkIf cfg.cec.pulse8.enable {
      description = "inputattach for pulse8-cec device on %I";
      bindsTo = [ "dev-%i.device" ];
      after = [ "dev-%i.device" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${pkgs.linuxConsoleTools}/bin/inputattach --pulse8-cec /dev/%I";
        Restart = "always";
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
    # connect to. State lives in /var/lib/tisvlay/daemon.toml
    # (StateDirectory), writable by the daemon so pairing can persist
    # credentials. Mode 0750 — only tisvlay user/group can read.
    # Clients talk to the daemon over the socket instead of reading
    # the file.
    systemd.services.tisvlay-daemon = {
      description = "tisvlay TV control daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      documentation = [ "https://github.com/TisVlay/TisVlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        SyslogIdentifier = "tisvlay-daemon";
        ExecStart = lib.escapeShellArgs (
          [
            (lib.getExe' cfg.package "tisvlay")
            "service"
            "daemon"
            "--socket"
            systemSocket
            "--config"
            "/var/lib/tisvlay/daemon.toml"
          ]
          ++ lib.optionals (configFile != null) [
            "--seed"
            "${configFile}"
          ]
          ++ lib.optionals (cfg.bindPort != null) [
            "--ssdp-port"
            (toString cfg.bindPort)
          ]
          # Grant the active DM's greeter group a socket ACL so the idle
          # client (dynamic-userdb on modern GDM) can reach the socket.
          ++ lib.optionals (cfg.greeter.enable && greeterGroup != null) [
            "--greeter-group"
            greeterGroup
          ]
          # The active-seat presence grant is the daemon's default; only
          # the opt-out is passed through.
          ++ lib.optionals (!cfg.seatAccess) [ "--seat-access=false" ]
          ++ cfg.extraArgs
        );
        # `systemctl reload` applies a hand edit of the store file (the
        # daemon treats SIGHUP as its reload rpc).
        ExecReload = "${pkgs.util-linux}/bin/kill -HUP $MAINPID";
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
      documentation = [ "https://github.com/TisVlay/TisVlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        SyslogIdentifier = "tisvlay-session";
        ExecStart = lib.escapeShellArgs (
          [
            (lib.getExe' cfg.package "tisvlay")
            "service"
            "session"
            "--system-socket"
            systemSocket
          ]
          ++ lib.optionals (cfg.sunshinePort != null) [
            "--sunshine-port"
            (toString cfg.sunshinePort)
          ]
        );
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
      documentation = [ "https://github.com/TisVlay/TisVlay" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      };
      serviceConfig = {
        SyslogIdentifier = "tisvlay-tray";
        ExecStart = lib.escapeShellArgs [
          (lib.getExe' cfg.package "tisvlay")
          "service"
          "tray"
          "--socket"
          systemSocket
        ];
        Restart = "on-failure";
        RestartSec = 5;
      };
    };

    # Greeter idle client. Runs as the greeter user inside the greeter
    # Wayland session — a systemd *user* manager started by the DM on the
    # session bus — so it MUST be a systemd.user unit: a system service
    # can't be WantedBy= the user-scoped greeter target and wouldn't inherit
    # the compositor's XDG_RUNTIME_DIR/WAYLAND_DISPLAY. It attaches to the
    # DM's greeter-only session target so it never starts in a real user
    # session; the greeter account(s) join the daemon group below for socket
    # access.
    systemd.user.services.tisvlay-greeter = lib.mkIf cfg.greeter.enable (
      lib.mkMerge [
        {
          description = "tisvlay greeter idle client";
          documentation = [ "https://github.com/TisVlay/TisVlay" ];
          environment = {
            RUST_LOG = cfg.logLevel;
          };
          serviceConfig = {
            SyslogIdentifier = "tisvlay-greeter";
            ExecStart = lib.escapeShellArgs [
              (lib.getExe' cfg.package "tisvlay")
              "service"
              "greeter"
              "--socket"
              systemSocket
            ];
            Restart = "on-failure";
            RestartSec = 5;
          };
        }
        # plasma-login: attach to its greeter Wayland target (KWin compositor,
        # ext-idle-notify). partOf stops us with the greeter session; the
        # target bindsTo the compositor, so a compositor death stops us too.
        (lib.mkIf plasmaLoginGreeter {
          after = [ "plasma-login-kwin_wayland.service" ];
          requires = [ "plasma-login-kwin_wayland.service" ];
          partOf = [ "plasma-login-wayland.target" ];
        })
        # GDM: the greeter runs `gnome-session --session=gnome-login`, so
        # gnome-session@gnome-login.target is its greeter-only session target
        # (the analogue of plasma-login-wayland.target). wantedBy it means we
        # start only at the login screen, in the gdm-greeter user's manager;
        # the greeter falls back to org.gnome.Mutter.IdleMonitor there.
        (lib.mkIf gdmGreeter {
          after = [ "gnome-session@gnome-login.target" ];
          partOf = [ "gnome-session@gnome-login.target" ];
          wantedBy = [ "gnome-session@gnome-login.target" ];
        })
      ]
    );

    # plasma-login attaches via a drop-in Wants= on its greeter target (a
    # vendor-shipped user unit; `asDropin` adds the Want without clobbering
    # it). GDM instead uses the unit's own wantedBy above, since
    # gnome-session@gnome-login.target is a template instance we don't own.
    systemd.user.targets."plasma-login-wayland" = lib.mkIf (cfg.greeter.enable && plasmaLoginGreeter) {
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
