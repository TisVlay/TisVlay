# tisvlay — Nix packaging

Flake-based Nix packaging for tisvlay: the Rust package derivation, the NixOS module, and an integration test that boots everything in a VM.

## Contents

| File | Purpose |
|---|---|
| [`package.nix`](package.nix) | `rustPlatform.buildRustPackage` derivation for the workspace. |
| [`module.nix`](module.nix) | NixOS module wiring the system daemon and both user services. |
| [`test.nix`](test.nix) | `pkgs.testers.runNixOSTest` — boots a VM with the module enabled and asserts daemon startup, D-Bus name ownership, `Manager1` surface, and `systemd-analyze` posture. |

Companion files outside this directory:

| File | Purpose |
|---|---|
| [`../systemd/tisvlay-daemon.service`](../systemd/tisvlay-daemon.service) | Distro (.deb/.rpm/.archlinux) unit file. Hardening kept in sync with `module.nix`. |
| [`../systemd/tisvlay-{session,tray}.service`](../systemd/) | Distro user-service unit files. Unhardened (see "User services" below). |
| [`../scripts/{post,pre}install.sh`](../scripts/) | Distro package lifecycle hooks — create the `tisvlay` system user, reload systemd / D-Bus. |

## Using the module

```nix
{
  inputs.tisvlay.url = "github:TisVlay/TisVlay";
  outputs = { nixpkgs, tisvlay, ... }: {
    nixosConfigurations.mybox = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        tisvlay.nixosModules.default
        { services.tisvlay.enable = true; }
      ];
    };
  };
}
```

Authoritative option reference: read [`module.nix`](module.nix) directly — every option has a docstring. Highlights:

- `services.tisvlay.logLevel` — maps to `RUST_LOG` on all three systemd units.
- `services.tisvlay.extraArgs` — appended to the daemon's `ExecStart`.
- `services.tisvlay.settings` — declarative seed config (TOML), rendered to the Nix store and passed as `--seed <path>`. Provisioning semantics: re-applied every start, ids already in the daemon-owned `/var/lib/tisvlay/daemon.toml` win, pairing still persists there — and a still-declared TV removed at runtime returns on the next start (drop the entry to drop the TV).
- `services.tisvlay.user` / `group` — defaults `"tisvlay"` / `"tisvlay"`. `uid` / `gid` default to `null`, letting NixOS pick a system UID and pin it via `/var/lib/nixos/uid-map`. Override only if you need the UID fixed across multiple hosts.

## Binary cache

Release builds of the flake's package are in the public [tisvlay.cachix.org](https://app.cachix.org/organization/tisvlay/cache/tisvlay) cache (public key `tisvlay.cachix.org-1:pa9AnLlywwXfm4cwUzRD1afQh2VbGTbYF6a9iQgzb/g=`), for x86_64-linux and aarch64-linux.

- **What's pushed:** `packages.${system}.default`, built from the flake's own `flake.lock`, and only its runtime closure. Build-only paths such as the vendored crate sources stay out, and so does anything cache.nixos.org already serves. Each build is pinned as `<tag>-<system>` (e.g. `v0.3.1-x86_64-linux`), keeping one revision, so eviction can't drop a release, and a re-pushed tag moves the pin to its new build. The module's default `package` builds against the host's nixpkgs, so it never matches the cache; using the cache is opt-in (the book's install guide, "Prebuilt binaries"). Users who keep the default build locally, as before.
- **When:** the `nix-cache` job in [`release.yml`](../../.github/workflows/release.yml) runs on every `v*` tag push, beside the distro package build. Nothing on `main` or in pull requests is pushed.
- **Access:** the push token (`CACHIX_AUTH_TOKEN`) lives in the `cachix` environment, which only admits `v*` tags. The job builds from cache.nixos.org alone and never substitutes from this cache, so it only writes to it.
- **Budget:** Cachix's free open-source plan holds 5 GB and evicts the least recently used paths past that. Paths already on cache.nixos.org aren't stored again.

## How the package builds

- `cargoLock.lockFile = ../../Cargo.lock` rather than `cargoHash`. Local workspace → no re-hashing needed on every dependency bump.
- `useNextest = true`. `cargoNextestHook` pulls in `cargo-nextest` automatically; `nativeCheckInputs` just needs `dbus` (the integration tests spawn a private `dbus-daemon`).
- Production build ships **default features only**, matching the `justfile`'s `package-build` recipe (cross-distro pipeline: see [`../README.md`](../README.md)). Tests still run with `--all-features` via `cargoTestFlags`.
- All internal `[workspace.dependencies]` entries carry `version = "0.1.0"` alongside `path = "..."` so `cargo package` can resolve the published manifest layout. See [`../../Cargo.toml`](../../Cargo.toml).

## Security posture

`tisvlay-daemon.service` is hardened to an `systemd-analyze security` score of **≤ 2.0** ("OK" band, currently 1.3).

Directive families applied in `module.nix`:

- **Filesystem/privilege**: `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `PrivateDevices`, `NoNewPrivileges`.
- **Network**: `RestrictAddressFamilies = AF_UNIX / AF_INET / AF_INET6 / AF_NETLINK`. Netlink is required for the carrier/route watcher (see [`../../docs/watchers/network.md`](../../docs/watchers/network.md)).
- **Kernel surface**: `ProtectKernelTunables/Modules/Logs`, `ProtectControlGroups`, `ProtectHostname`, `ProtectClock`, `ProtectProc=invisible`, `ProcSubset=pid`.
- **Syscalls**: `SystemCallArchitectures=native`, `SystemCallFilter=@system-service ~@privileged ~@resources`.
- **Capabilities**: `CapabilityBoundingSet=`, `AmbientCapabilities=` (empty → drop all). Daemon runs as unprivileged system user.
- **Hygiene**: `LockPersonality`, `MemoryDenyWriteExecute`, `RestrictRealtime`, `RestrictSUIDSGID`, `RestrictNamespaces`, `UMask=0077`, `RemoveIPC`.

Remaining deductions in the ~1.3 residual are intrinsic to a network-facing daemon: `PrivateNetwork` can't be `true` (needs to reach TVs), `AF_INET*` must stay allowed (WebSocket + WoL), `IPAddressDeny` isn't practical across heterogeneous home networks.

### Checking locally

```bash
# Score the generated system unit:
systemd-analyze security --offline=true \
  "$(nix eval --impure --raw --expr '
    (import <nixpkgs> {}).nixos {
      imports = [ ./packaging/nix/module.nix ];
      services.tisvlay.enable = true;
      # ... stubs required by nixos builder: stateVersion, boot, fs
    }' ).config.systemd.units."tisvlay-daemon.service".unit"/tisvlay-daemon.service

# Or score the distro unit directly:
systemd-analyze security --offline=true packaging/systemd/tisvlay-daemon.service
```

## VM test

```bash
nix flake check                              # full check suite, includes VM
nix build .#checks.x86_64-linux.vm --no-link # VM test in isolation
```

The test boots a minimal NixOS VM with `services.tisvlay.enable = true` and asserts:

1. `tisvlay-daemon.service` reaches `active (running)`.
2. `org.tisvlay.Daemon` is owned on the system bus.
3. `Manager1` introspection at `/org/tisvlay/Manager` exposes the expected interface.
4. `GetSupportedVendors` returns the LG backend (mock is feature-gated off in production builds).
5. `ObjectManager.GetManagedObjects` is empty on a fresh install.
6. `tisvlay-session.service` and `tisvlay-tray.service` user unit files are loaded.
7. `/var/lib/tisvlay/` exists with owner `tisvlay:tisvlay`.
8. `tisvlay` user exists with a system-range UID (< 1000). Exact value is NixOS-allocated unless pinned via `services.tisvlay.uid`.
9. `systemd-analyze security tisvlay-daemon.service` score ≤ 2.0. The full breakdown is logged via `machine.log()` so the VM build log preserves which settings pass / fail without a manual re-run.

**Deliberately not covered**: `tisvlay-session` / `tisvlay-tray` auto-start via `graphical-session.target`. Exercising that path needs a real display manager in the VM. The units are verified to load (they are reachable via `systemctl --global`) but not executed.

## Consistency with distro packaging

These hardening directives are mirrored in [`../systemd/tisvlay-daemon.service`](../systemd/tisvlay-daemon.service), so .deb/.rpm/.archlinux installs hit the same `systemd-analyze` score as NixOS.

NixOS-only (not backported):

- **UID/GID pinning** via the module option. Distros use `adduser --system` in [`postinstall.sh`](../scripts/postinstall.sh); the assigned UID varies by distro convention.
- **Declarative `settings` rendering**. Distros expect a mutable `/var/lib/tisvlay/config.toml` managed by the pairing wizard.

**If you change hardening in one place, change both.** The files carry a comment pointing at their sibling for this reason.

## User services

`systemd.user.services.tisvlay-session` and `.tisvlay-tray` are intentionally left with the default systemd sandbox posture. They need access to the Wayland socket, `ksni` tray backend, and user D-Bus session — broader confinement risks breaking tray rendering or session-event monitoring on the wide variety of desktop environments we target.

Same holds for the distro user-service files.

## Limitations and future work

- **Not published to nixpkgs.** The module option `package.defaultText` is currently `literalExpression "pkgs.callPackage ./package.nix { }"`. Upstream integration would require a maintainer entry in `nixos/maintainers/maintainer-list.nix`, a UID reservation in `nixos/modules/misc/ids.nix`, and a NixOS release note.
- **Modular Services migration** (the `_class = "service"` pattern new in NixOS 25.11) is deferred. The API is still evolving; revisit when it graduates from "in development" and we have a concrete nix-darwin or Home Manager consumer.
- **SSDP discovery in the firewall.** Deferred per [`../../docs/todo.md`](../../docs/todo.md); SSDP needs an inbound UDP allow rule on the daemon's chosen network interface, which requires extra option plumbing.

## Related documentation

- [`../README.md`](../README.md) — packaging pipeline overview (Nix vs distro vs container channels, build-pipeline rationale, rejected alternatives).
- [`../../.claude/CLAUDE.md`](../../.claude/CLAUDE.md) — project structure and "Quick Reference" testing table.
- [`../../docs/design.md`](../../docs/design.md) — architecture, D-Bus interface, config schema.
- [`../../docs/testing.md`](../../docs/testing.md) — test harness, mock backend, coverage, observability.
- [`../../docs/firewall.md`](../../docs/firewall.md) — SSDP firewall notes.
