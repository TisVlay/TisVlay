# Nix Packaging — Alignment with Nixpkgs Conventions

Scope: bring [packaging/nix/package.nix](../packaging/nix/package.nix) and
[packaging/nix/module.nix](../packaging/nix/module.nix) up to the standards
described in the nixpkgs docs (CONTRIBUTING.md, rust.section.md,
service-mgmt.chapter.md, modular-services.md, service/README.md,
README-modular-services.md).

This plan is ordered by priority — each phase is independently landable.

---

## Phase 1 — Low-risk cleanups (no behavior change) ✅

Goal: correct obvious issues, small diff, easy review.

- [x] **package.nix**: remove `longDescription = "TODO"`. `description`
      alone is sufficient; placeholder text should not ship.
- [x] **package.nix**: add `meta.maintainers` (literal attrset while
      out-of-tree). Required by README-modular-services.md §"Minimum
      Standard" for anything that exposes a NixOS module.
- [x] **package.nix**: add `meta.changelog = "https://github.com/ruffsl/tisvlay/releases"`
      (or a per-version anchor once tags exist).
- [x] **module.nix**: add top-level `meta.maintainers`.
- [x] **module.nix**: add `documentation = [ "https://github.com/ruffsl/tisvlay" ];`
      to each of the three systemd units (system daemon + user services).

**Commit**: `packaging/nix: meta cleanup (maintainers, changelog, drop TODO)`

---

## Phase 2 — Rust packaging idioms ✅ (with follow-up in 2.5)

Goal: follow rust.section.md guidance for in-tree projects; eliminate
`cargoHash` churn and the custom `checkPhase`.

- [x] **package.nix**: replace `cargoHash` with
      `cargoLock.lockFile = ../../Cargo.lock`.
      Rationale (rust.section.md §"Importing a Cargo.lock file"):
      `cargoHash` forces a re-hash after every `Cargo.lock` change; for an
      in-tree build the lockfile import is the documented alternative.
- [x] **package.nix**: drop the custom `checkPhase` in favor of
      `useNextest = true`. Per rust.section.md §"Using `cargo-nextest`",
      the `cargoNextestHook` pulls in `cargo-nextest` automatically, so
      it was removed from `nativeCheckInputs` (`dbus` stays — it is
      needed for integration tests that spawn a private `dbus-daemon`).
- [x] Verified sandboxed build: `nix build .` succeeds end-to-end on
      Linux (`/nix/store/...-tisvlay-0.1.0.drv` builds clean).

**Commit**: `packaging/nix: use cargoLock.lockFile and useNextest`

---

## Phase 2.5 — Align Nix features with distro packages (bug fix + workspace tuning) ✅

Landed via Plan 2 Phase F (commit `998cd6e`, PR #28) and the greenfield
hygiene PR (#29). All three packaging targets (Nix, musl release,
.deb/.rpm/.archlinux) now produce the same artifact from default
features.

### Part A — Fix Nix feature flags ✅

- [x] **package.nix**: removed `cargoBuildFlags = [ "--all-features" ]`;
      kept `cargoTestFlags = [ "--all-features" ]`. Production binary
      no longer ships the mock vendor.

### Part B — Workspace `Cargo.toml` tuning ✅

- [x] `[profile.release]` added: `lto = "thin"`, `codegen-units = 1`,
      `strip = "symbols"`.
- [x] `[workspace.package] publish = false` default; every crate
      inherits via `publish.workspace = true`. `cargo metadata` reports
      `"publish":[]` uniformly.

### Bonus (PR #29) — Edition / resolver / MSRV + internal dep versions

Out of scope for the original phase but landed in the same period:

- [x] `edition = "2024"`, `resolver = "3"`, `rust-version = "1.85"`
      (floor for edition 2024 + resolver 3).
- [x] `[workspace.dependencies]` internal path entries gained
      `version = "0.1.0"` alongside `path = "..."` — prerequisite for
      `cargo package` / `cargo publish`.

### Still open (follow-up)

- [x] Centralize lints with `[workspace.lints]`. Policy:
      `clippy::undocumented_unsafe_blocks`,
      `rust::missing_debug_implementations`,
      `rust::unreachable_pub` — all at warn level, promoted to deny
      via `cargo clippy -- -D warnings` in the justfile.
- [x] Fix `missing_debug_implementations` warnings (36 sites). Most
      via `#[derive(Debug)]`; 6 via manual impl (types holding
      `dyn Trait` / opaque zbus / WebSocket internals use
      `finish_non_exhaustive()`).
- [x] Fix `unreachable_pub` warnings (21 sites). All resolved by
      `cargo fix` (pub → pub(crate) on internal-only items in
      `tisvlay-lg/connection.rs`, `endpoints.rs`, and
      `tisvlay-tray/state.rs`, `tray.rs`). No items turned out to be
      intentionally pub-for-external-use.

---

## Phase 3 — Module configurability ✅

Goal: stop hardcoding everything; give downstream users the knobs a
typical nixpkgs module exposes.

- [x] **module.nix**: expose options
  - [x] `services.tisvlay.logLevel` (enum: trace/debug/info/warn/error)
        — maps to `RUST_LOG` on all three systemd units.
  - [x] `services.tisvlay.extraArgs` (list of strings, appended to
        `tisvlayd`'s `ExecStart`).
  - [x] `services.tisvlay.settings` (using `pkgs.formats.toml { }`)
        for declarative TOML config, rendered to the Nix store and
        passed via `--config`. Default `null` keeps the
        `StateDirectory`-backed mutable path (so `tisvlay pair`
        continues to work). Docstring explicitly calls out the
        pairing-persistence tradeoff when settings is non-null.
  - [x] `services.tisvlay.user` / `services.tisvlay.group` (default
        `"tisvlay"`) for sites that need to override.
- [x] **module.nix**: stable UID/GID `979` on `users.users.tisvlay`
      and `users.groups.tisvlay`. Configurable via
      `services.tisvlay.uid` / `gid` options. Docstring notes upstream
      nixpkgs would reassign via `nixos/modules/misc/ids.nix`.
- [x] **module.nix**: `settings` wires into `ExecStart` as
      `--config <nix-store-path>` when non-null. `extraArgs` still
      appends after, so users can override.
- [x] **module.nix**: `defaultText` on the `package` option now reads
      `literalExpression "pkgs.callPackage ./package.nix { }"` — the
      actual default, not the fictional `pkgs.tisvlay`.

**Verified via** `pkgs.nixos` eval: default / settings-only /
settings + extraArgs all produce the expected `ExecStart` strings;
`RUST_LOG` propagates; `users.users.tisvlay.uid = 979`.

**Commits**:
- `packaging/nix: add logLevel, extraArgs, user/group options + stable UID`
- `packaging/nix: add declarative settings option`

---

## Phase 4 — NixOS VM test ✅

Goal: meet README-modular-services.md §"Minimum Standard" ("Modular
services MUST be accompanied by a NixOS VM test"). Even for a classic
module, a VM test is the strongest evidence of correctness per
CONTRIBUTING.md §"Tested via one or more NixOS test(s)".

- [x] Added [packaging/nix/test.nix](../packaging/nix/test.nix) using
      `pkgs.testers.runNixOSTest`. It:
  - imports the module, sets `services.tisvlay.enable = true`
  - waits for `multi-user.target` and `tisvlayd.service`
  - verifies `systemctl is-active tisvlayd.service`
  - verifies the D-Bus name `org.tisvlay.Daemon` is owned
  - introspects `Manager1` at `/org/tisvlay/Manager`
  - calls `GetSupportedVendors` and asserts `"lg"` is returned
  - calls `ObjectManager.GetManagedObjects` and asserts zero TVs on a
    fresh install
  - verifies `tisvlay-session.service` and `tisvlay-tray.service`
    user unit files are loaded
  - verifies `/var/lib/tisvlay/` exists with owner `tisvlay:tisvlay`
  - verifies the configured stable UID (979) is applied
- [x] Wired into `flake.nix` as `checks.${system}.vm`. Running
      `nix flake check` or `nix build .#checks.x86_64-linux.vm`
      boots the VM and runs the test.

**Not covered** (intentional, out of scope here):
- Auto-start of user services via `graphical-session.target` — needs
  a real display manager in the VM. Unit files are loaded and
  explicitly startable via `systemctl --user`; the full graphical
  boot path is left for a follow-up once a tray integration test is
  authored (see Plan 2 Phase E placeholder).
- `passthru.tests.vm` on the package — deferred; `checks.vm` covers
  `nix flake check` usage which is the primary CI entry.

**Commit**: `packaging/nix: add NixOS VM test covering daemon startup`

---

## Phase 5 — Security hardening ✅

Applied a baseline suitable for a long-running daemon that only talks
D-Bus, TCP/UDP (WebSocket + WoL), and netlink.

- [x] **module.nix**: tightened
      `systemd.services.tisvlayd.serviceConfig`. Added:
  - `RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" "AF_NETLINK" ]`
    — netlink kept for carrier/route watching; no PACKET / BLUETOOTH.
  - `SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ]`
    and `SystemCallArchitectures = "native"`.
  - `CapabilityBoundingSet = [ "" ]` and `AmbientCapabilities = [ "" ]`.
  - `LockPersonality`, `MemoryDenyWriteExecute`, `RestrictRealtime`,
    `RestrictSUIDSGID`, `RestrictNamespaces`.
  - Kernel surface lockdown: `ProtectKernelTunables`,
    `ProtectKernelModules`, `ProtectKernelLogs`,
    `ProtectControlGroups`, `ProtectHostname`, `ProtectClock`,
    `ProtectProc = "invisible"`, `ProcSubset = "pid"`.
  - `PrivateDevices = true` (daemon talks network, not /dev nodes).
- [x] Validated via the VM test. Added an assertion in
      [test.nix](../packaging/nix/test.nix) that parses
      `systemd-analyze security tisvlayd.service` and fails the test
      if the overall exposure level exceeds `2.0`. Current run
      passes → regression guard against accidental hardening loss in
      future edits.

**Out of scope / deferred**:
- Matching hardening on `tisvlay-session` / `tisvlay-tray`. User
  services need broader access (Wayland socket, ksni tray backend,
  etc.). Apply when/if there's a concrete threat model for user-bus
  attacks — not required for the system-daemon hardening goal.

**Commit**: `packaging/nix: tighten tisvlayd systemd sandboxing + VM assertion`

---

## Phase 6 — (Optional, deferred) Modular Services migration

Status per modular-services.md: "new in NixOS 25.11 … significant
changes should be expected." Not worth doing until the API stabilizes
**and** we have an external consumer who would benefit from portability.

Not planned. Revisit if/when:
- modular-services graduates from "in development"
- we want `nix-darwin` or Home Manager consumers
- the package is upstreamed to nixpkgs

---

## Out of scope (tracked separately)

- NixOS module firewall/SSDP discovery — deferred per
  [docs/todo.md](todo.md).
- Publishing to nixpkgs — requires Phase 1-4 complete + upstream-ready
  UID allocation + maintainer entry in `maintainer-list.nix`.
