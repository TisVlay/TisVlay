# Review: Multi-distro packaging and crates.io prep (17628ae..bfe759a)

Review of 14 commits covering multi-distro packaging via cargo-nfpm, systemd service
hardening, musl static builds, GitHub Actions release workflow, and ring crypto backend.

Build: clean. Tests: all pass.

---

## W1. Systemd service files — significantly improved

**Files:** `systemd/tisvlayd.service`, `systemd/org.tisvlay.Daemon.service`,
`systemd/tisvlay-session.service`, `systemd/tisvlay-tray.service`

Key improvements:
- Daemon runs as dedicated `tisvlay` user (was root)
- `StateDirectory=tisvlay` for config at `/var/lib/tisvlay/` (was `ReadWritePaths=/etc/tisvlay`)
- `ProtectHome=true` (was `read-only` — `true` is stricter)
- D-Bus activation uses `SystemdService=tisvlayd.service` so activation goes through
  the systemd unit (gets `StateDirectory`, security hardening, etc.)
- `Slice=session.slice` removed from user services (unnecessary — they're already in
  `user.slice` by default)
- Tray gains `After=tisvlay-session.service` for profile data availability at startup
- Description updated from "LG WebOS" to vendor-agnostic "tisvlay"

The `/var/lib/tisvlay/` state directory works on immutable root filesystems (Silverblue,
Bazzite) where `/etc/tisvlay/` would fail. Good forward-thinking.

**Severity:** Info (correct and well-hardened)

---

## W2. D-Bus activation with `SystemdService=` — fixes state dir issue

**File:** `systemd/org.tisvlay.Daemon.service`

```
[D-BUS Service]
Name=org.tisvlay.Daemon
Exec=/usr/bin/tisvlayd
User=tisvlay
SystemdService=tisvlayd.service
```

Without `SystemdService=`, D-Bus activation runs `Exec=` directly, bypassing the
systemd unit. This skips `StateDirectory=tisvlay` (directory never created) and all
security hardening directives. With `SystemdService=`, D-Bus tells systemd to start
the service unit instead, which creates the directory and applies all sandboxing.

The `Exec=` line is retained as a fallback for systems without systemd.

**Severity:** Info (correct fix for a real production issue)

---

## W3. `ring` instead of `aws-lc-sys` for musl compatibility

**File:** `Cargo.toml`

```toml
rustls = { version = "0.23", default-features = false, features = ["std", "ring"] }
```

`aws-lc-sys` links against glibc-specific symbols (`__fprintf_chk`, `__memcpy_chk`)
that don't exist in musl. Switching to `ring` as the crypto backend enables static
musl builds for portable distro packages.

`ring` is a well-maintained Rust crypto library used by many production systems. The
performance difference from `aws-lc-rs` is negligible for TLS (WebSocket connections
to LG TVs don't need AWS-scale throughput).

The `Cargo.lock` confirms `aws-lc-rs` and `aws-lc-sys` are removed, along with their
build dependencies (`cmake`, `dunce`, `fs_extra`). This also speeds up builds by
removing a C/cmake compilation step.

**Severity:** Info (correct tradeoff, removes build complexity)

---

## W4. cargo-nfpm config on tisvlayd — clean packaging

**File:** `crates/tisvlayd/Cargo.toml`

The `[package.metadata.nfpm]` section bundles all 4 binaries, 3 systemd units, and
2 D-Bus files into a single `tisvlay` package. The `-p tisvlayd` flag tells cargo-nfpm
where to read the config; the output package name is set inside.

`contents` correctly maps:
- Binaries to `/usr/bin/`
- System service to `/usr/lib/systemd/system/`
- User services to `/usr/lib/systemd/user/`
- D-Bus activation to `/usr/share/dbus-1/system-services/`
- D-Bus policy to `/etc/dbus-1/system.d/` with `type = "config"` (preserved on upgrade)

Distro-specific deps: `libc6` for deb, `glibc` for rpm. Both are always present but
declaring them is good practice for package manager dependency resolution.

**Severity:** Info (correct packaging config)

---

## W5. Install scripts — correct lifecycle management

**Files:** `packaging/scripts/postinstall.sh`, `packaging/scripts/preremove.sh`

**postinstall.sh:**
- Creates `tisvlay` system user/group (idempotent — checks existence first)
- Creates state directory `/var/lib/tisvlay/` with correct ownership as fallback
- Reloads systemd and D-Bus (handles both `systemctl reload dbus` and `dbus-send` fallback)
- Enables but doesn't start the daemon (user needs to pair a TV first)
- Prints helpful next-steps message

**preremove.sh:**
- Stops and disables the daemon
- Intentionally preserves the `tisvlay` user, group, and `/var/lib/tisvlay/` (contains
  TV credentials). Documents manual cleanup commands.

The credential preservation is the right call — accidental credential loss during
package upgrade would be worse than leftover files after uninstall.

**Severity:** Info (correct lifecycle handling)

---

## W6. Release workflow — draft releases on v* tags

**File:** `.github/workflows/release.yml`

Pipeline: checkout -> toolchain -> cache -> install cargo-nfpm -> build (release) ->
package (deb, rpm, archlinux) -> checksums -> draft release with assets.

- `permissions: contents: write` for GitHub release API access
- `fail_on_unmatched_files: true` catches packaging failures
- `draft: true` so releases are reviewed before publishing
- SHA256 checksums generated and included as a release asset

One note: the release build uses the runner's glibc (not musl), while the VS Code
tasks use musl for portable builds. This is fine for the release workflow since CI
targets Ubuntu (glibc), but the deb/rpm packages won't work on Alpine or other musl
distros. For broader compatibility, the release workflow could use musl (matching the
local packaging tasks), but that's a future enhancement.

**Severity:** Info (correct for current target distros)

---

## W7. Musl devenv in flake.nix — NixOS-friendly cross-compilation

**File:** `flake.nix`

```nix
rust = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
  targets = [ "x86_64-unknown-linux-musl" ];
};
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";
```

Adds the musl target to the Rust toolchain and configures the musl linker via env var.
The `rust-toolchain.toml` stays clean (no musl target) so CI isn't affected. Only the
Nix devShell gets musl support.

The VS Code task `package: build (musl)` symlinks `target/release` to the musl output
directory so cargo-nfpm's `target/release/` paths work without modification.

**Severity:** Info (clean cross-compilation setup)

---

## W8. `nix/` to `packaging/nix/` — clean reorganization

The Nix packaging files moved under `packaging/` alongside the new `scripts/` directory.
`flake.nix` imports updated. Design.md and CLAUDE.md references updated. The packaging
directory now has a clear structure:

```
packaging/
  nix/
    package.nix
    module.nix
  scripts/
    postinstall.sh
    preremove.sh
```

**Severity:** Info (good directory organization)

---

## W9. Stale symlink removal before musl build

**Commit:** d3afeb8

The `package: build (musl)` task removes the `target/release` symlink before building
to prevent cargo from locking on a stale path. Without this, a previous build's symlink
could point to a non-existent directory, causing cargo to fail.

```bash
rm -f target/release 2>/dev/null; cargo build ...
```

**Severity:** Info (correct build hygiene)

---

## W10. Plan is thorough and mostly complete

**File:** `docs/plan.md`

Phases 1-5 are checked off. Remaining:
- Update design.md and CLAUDE.md (partially done in 74fc288)
- Merge to main, tag v0.1.0, verify release workflow

The plan documents the design decisions well (why no meta crate, why cargo-nfpm, why
crates.io and distro packaging are separate concerns). References real-world patterns
(spotifyd, kanidm, greetd).

**Severity:** Info (well-documented plan)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| W1 | **Info** | Systemd services significantly hardened, vendor-agnostic |
| W2 | **Info** | D-Bus activation via SystemdService fixes state dir issue |
| W3 | **Info** | ring crypto backend enables musl, removes cmake build step |
| W4 | **Info** | cargo-nfpm config correctly bundles all components |
| W5 | **Info** | Install scripts handle lifecycle correctly, preserve credentials |
| W6 | **Info** | Release workflow creates draft releases with checksums |
| W7 | **Info** | Musl devenv in flake.nix is clean cross-compilation setup |
| W8 | **Info** | packaging/ directory reorganization is clean |
| W9 | **Info** | Stale symlink removal prevents build failures |
| W10 | **Info** | Plan is thorough with remaining checklist items clear |

## Overall Assessment

Clean packaging infrastructure that covers three distribution channels: NixOS (flake),
multi-distro (cargo-nfpm for deb/rpm/archlinux), and crates.io (per-crate publishing).

The systemd service improvements are the most impactful change beyond packaging itself --
running as a dedicated user with StateDirectory on `/var/lib/tisvlay/` is correct for
production deployments and works on immutable root filesystems.

The ring crypto backend switch is a pragmatic choice that removes a C build dependency
and enables musl static builds. The performance tradeoff is negligible for the use case.

No actionable items. All 10 findings are info-level.
