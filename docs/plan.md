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
