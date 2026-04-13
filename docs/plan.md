# Nix Packaging — Alignment with Nixpkgs Conventions

Scope: bring [packaging/nix/package.nix](../packaging/nix/package.nix) and
[packaging/nix/module.nix](../packaging/nix/module.nix) up to the standards
described in the nixpkgs docs (CONTRIBUTING.md, rust.section.md,
service-mgmt.chapter.md, modular-services.md, service/README.md,
README-modular-services.md).

This plan is ordered by priority — each phase is independently landable.

---

## Phase 1 — Low-risk cleanups (no behavior change)

Goal: correct obvious issues, small diff, easy review.

- [ ] **package.nix**: remove `longDescription = "TODO"`. `description`
      alone is sufficient; placeholder text should not ship.
- [ ] **package.nix**: add `meta.maintainers = [ lib.maintainers.ruffsl ]`
      (or a literal attrset `[{ name = "ruffsl"; github = "ruffsl"; ... }]`
      while out-of-tree). Required by README-modular-services.md §"Minimum
      Standard" for anything that exposes a NixOS module.
- [ ] **package.nix**: add `meta.changelog = "https://github.com/ruffsl/tisvlay/releases"`
      (or a per-version anchor once tags exist).
- [ ] **module.nix**: add `meta.maintainers = [ lib.maintainers.ruffsl ];`
      at the top level of the module.
- [ ] **module.nix**: add `documentation = [ "https://github.com/ruffsl/tisvlay" ];`
      to each of the three `serviceConfig` blocks (system daemon + user
      services).

**Commit**: `packaging/nix: meta cleanup (maintainers, changelog, drop TODO)`

---

## Phase 2 — Rust packaging idioms

Goal: follow rust.section.md guidance for in-tree projects; eliminate
`cargoHash` churn and the custom `checkPhase`.

- [ ] **package.nix**: replace
      ```nix
      cargoHash = "sha256-...";
      ```
      with
      ```nix
      cargoLock.lockFile = ../../Cargo.lock;
      ```
      Rationale (rust.section.md §"Importing a Cargo.lock file"):
      `cargoHash` forces a re-hash after every `Cargo.lock` change; for an
      in-tree build the lockfile import is the documented alternative.
- [ ] **package.nix**: drop the custom `checkPhase`; set
      ```nix
      useNextest = true;
      ```
      instead. Per rust.section.md §"Using `cargo-nextest`", this is the
      supported path — inherits parallel tests, `--no-fail-fast`, feature
      plumbing (`cargoCheckFeatures`, `checkFlags`). Keep `cargo-nextest`
      and `dbus` in `nativeCheckInputs`.
- [ ] **package.nix**: consider `cargoCheckFeatures` / `checkFeatures` if
      there's a feature subset we actually want to exercise under Nix
      sandbox (e.g. excluding network-bound SSDP discovery tests).
- [ ] Verify sandboxed build locally: `nix build .#tisvlay -L`
      (CONTRIBUTING.md §"Tested using sandboxing" — sandboxed builds are
      the expected baseline on Linux).

**Commit**: `packaging/nix: use cargoLock.lockFile and useNextest`

---

## Phase 3 — Module configurability

Goal: stop hardcoding everything; give downstream users the knobs a
typical nixpkgs module exposes.

- [ ] **module.nix**: expose options
  - `services.tisvlay.logLevel` (enum: trace/debug/info/warn/error)
  - `services.tisvlay.extraArgs` (list of strings, appended to `ExecStart`)
  - `services.tisvlay.settings` (using `(pkgs.formats.toml { }).generate`)
    so users can manage `config.toml` declaratively instead of relying
    solely on `StateDirectory` mutability.
  - `services.tisvlay.user` / `services.tisvlay.group` (default `tisvlay`)
    for sites that need to override.
- [ ] **module.nix**: allocate a stable system UID/GID. While out-of-tree,
      pick one in the documented user-assigned range (e.g. in the
      `services` block, set `users.users.tisvlay.uid = <N>` and
      `users.groups.tisvlay.gid = <N>`). If this ever goes upstream, the
      UID must be assigned in `nixos/modules/misc/ids.nix`.
- [ ] **module.nix**: if `settings` is set, wire it into `ExecStart` via a
      `--config` flag (or drop a file into `/etc/tisvlay/config.toml`) so
      the daemon reads declarative config rather than the
      `StateDirectory` copy.
- [ ] **module.nix**: `defaultText` on the `package` option should remain
      accurate — document that this module is out-of-tree until (if) it
      lands in nixpkgs.

**Commit**: `packaging/nix: expose logLevel, extraArgs, settings options`

---

## Phase 4 — NixOS VM test

Goal: meet README-modular-services.md §"Minimum Standard" ("Modular
services MUST be accompanied by a NixOS VM test"). Even for a classic
module, a VM test is the strongest evidence of correctness per
CONTRIBUTING.md §"Tested via one or more NixOS test(s)".

- [ ] Add `packaging/nix/test.nix` using `nixosTest { ... }` (or
      `pkgs.testers.runNixOSTest`) that:
  - imports the module
  - sets `services.tisvlay.enable = true`
  - asserts `tisvlayd.service` reaches `active (running)`
  - asserts the D-Bus name `org.tisvlay.Daemon` is owned
  - calls `busctl --system call ... Manager1 GetSupportedVendors` and
    checks the expected `mock`/`lg` vendors are returned
  - spins up a user session, starts `tisvlay-session` and `tisvlay-tray`
    under that user, asserts both are active
- [ ] Expose the test as `passthru.tests.vm` on the package so
      `nix build .#tisvlay.tests.vm` runs it.
- [ ] Wire the test into `flake.nix` checks (`checks.x86_64-linux.vm =
      ...`) so `nix flake check` exercises it.

**Commit**: `packaging/nix: add NixOS VM test covering daemon + user services`

---

## Phase 5 — Security hardening (nice-to-have)

Not required for conformance, but worth doing before advertising the
module to external users.

- [ ] **module.nix**: tighten `systemd.services.tisvlayd.serviceConfig`:
  - `RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" "AF_NETLINK" ]`
    (netlink is required for the carrier/route watcher)
  - `SystemCallFilter = [ "@system-service" ]`
  - `SystemCallArchitectures = "native"`
  - `CapabilityBoundingSet = [ "" ]` (empty — daemon needs no caps)
  - `LockPersonality = true`
  - `MemoryDenyWriteExecute = true`
  - `RestrictRealtime = true`
  - `RestrictSUIDSGID = true`
- [ ] Validate with `systemd-analyze security tisvlayd.service` — aim for
      exposure level ≤ 2.0.

**Commit**: `packaging/nix: tighten tisvlayd systemd sandboxing`

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
