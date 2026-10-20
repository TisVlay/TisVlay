# tisvlay — packaging

High-level overview of every channel we ship through, why the build pipeline is shaped the way it is, and what to know before changing it.

## Channels

| Channel | Source of truth | Use case |
|---|---|---|
| **Nix flake / NixOS module** | [`nix/package.nix`](nix/package.nix), [`nix/module.nix`](nix/module.nix) | NixOS hosts, declarative service config. See [`nix/README.md`](nix/README.md) for the full deep-dive on the Nix derivation, hardening posture, VM test, and module options. |
| **`.deb` / `.rpm` / `.pkg.tar.zst`** (this directory) | [`crates/tisvlay-cli/Cargo.toml`](../crates/tisvlay-cli/Cargo.toml) `[package.metadata.nfpm*]` + [`justfile`](../justfile) `package-*` recipes | Debian/Ubuntu, Fedora/RHEL, Arch Linux. One bundled package per format containing all five binaries plus systemd units, the GUI desktop entry, and the CLI's shell completions. |
| **Test container** | [`docker/Dockerfile`](docker/Dockerfile) | Reproducible Ubuntu environment for verifying the build matrix without touching CI, and for smoke-testing the resulting `.rpm`/`.deb` against a clean glibc target. |
| **Dev-shell native build** | [`flake.nix`](../flake.nix) `devShells.default` | `cargo run` / `cargo test` / `just check` directly from the Nix dev shell. **Not** what produces shippable binaries — see "Build pipeline" below. |

The rest of this document is about the second channel — the cross-distro `.deb`/`.rpm`/`.pkg.tar.zst` produced by `just package-{deb,rpm,archlinux,all}`.

## Build pipeline

```
┌─────────────────────────────┐
│ NixOS dev shell             │
│   rust-toolchain.toml       │
│   cargo-zigbuild + zig      │   linker = zig cc
│   nfpm (Go binary)          │   target  = <arch>-unknown-linux-gnu.<glibc_baseline>
└──────────────┬──────────────┘
               │  just package-build
               ▼
┌─────────────────────────────┐
│ target/<arch>-unknown-linux │
│        -gnu/dist/            │
│   tisvlay (CLI + services),  │   ELF interpreter:
│   tisvlay-gui                │     /lib64/ld-linux-x86-64.so.2
│                              │   glibc symbols: ≥ glibc_baseline
└──────────────┬──────────────┘
               │  cargo nfpm package -p tisvlay-cli ... -f deb|rpm|archlinux
               ▼
┌─────────────────────────────┐
│ tisvlay_<ver>_<deb arch>.deb│
│ tisvlay-<ver>.<arch>.rpm    │   Runs on any distro at or newer than
│ tisvlay-<ver>-...pkg.tar.zst │   `glibc_baseline` declared in justfile.
└─────────────────────────────┘
```

The two ingredients that make this work cleanly from a NixOS host are:

- **cargo-zigbuild** invokes `cargo` with zig as the C/C++ linker. Zig's standard library bundles glibc shim libraries for many versions; passing `--target x86_64-unknown-linux-gnu.<version>` tells it to link only symbols available at-or-before that glibc version. The ELF interpreter is the standard `/lib64/ld-linux-x86-64.so.2` (no Nix-store path), and the dynamic-section `NEEDED` entries name plain SONAMEs (`libc.so.6`, `libfontconfig.so.1`, …). The result loads on any distro with glibc ≥ baseline without further tooling.
- **nfpm** (the Go binary, invoked through the `cargo-nfpm` plugin) reads `[package.metadata.nfpm*]` from [`crates/tisvlay-daemon/Cargo.toml`](../crates/tisvlay-daemon/Cargo.toml) and produces a `.deb`/`.rpm`/`.pkg.tar.zst` from the same source-of-truth. Per-format `depends` lists declare runtime libraries the package manager must pull in.

### Glibc baseline

Set in [`justfile`](../justfile) as `glibc_baseline := "2.34"`. Coverage as of 2026-05-03:

| baseline | covers (distros with glibc ≥ baseline, in-support) | notes |
|---|---|---|
| 2.28 | RHEL / AlmaLinux / Rocky 8 (maint. until 2029-05); everything newer | [`manylinux_2_28`](https://github.com/pypa/manylinux) floor. Enterprise-only — no consumer desktop distro still ships this. |
| 2.31 | + Debian 11 (LTS until 2026-08), Ubuntu 20.04 (ESM-only since 2025-04) | Adds only legacy/paid users beyond what 2.34 covers; Debian 11 LTS exits in three months. |
| **2.34** | + RHEL / AlmaLinux / Rocky 9 (maint. until 2032-05), Ubuntu 22.04 LTS (until 2027-04), Fedora 35+ | Currently configured. Every in-standard-support enterprise + desktop distro. Clear of Zig's older-anchor undefined-symbol bugs. |
| 2.36 | Debian 12 (LTS until 2028-06), Ubuntu 24.04+, Fedora 36+, RHEL 10+ — drops RHEL 9 | Drops RHEL 9 (maint. through 2032) for no real-world gain. Avoid. |
| 2.39 | RHEL 10 (since 2025-05), Debian 13 (since 2025-08), Ubuntu 24.04+, Fedora 41+ — drops RHEL 9, Ubuntu 22.04 | Too aggressive for a 2026 release. |

Bumping the baseline is a one-line change in `glibc_baseline`. The 2.34 anchor was picked because Ubuntu 20.04 went ESM-only in April 2025 and Debian 11 LTS exits August 2026, so 2.31 (and below) now adds only legacy/paid users — while keeping the bar at 2.34 still covers RHEL 9 active through 2032. Going below 2.28 starts hitting [zig #15486](https://github.com/ziglang/zig/issues/15486), [#5882](https://github.com/ziglang/zig/issues/5882), [#9485](https://github.com/ziglang/zig/issues/9485) (undefined `__isoc23_strtoul`, `fcntl64`, `aligned_alloc`).

### Runtime dependencies

`tisvlay-gui` is the only binary that pulls non-glibc deps. Two categories:

- **Link-time (`NEEDED` in the ELF)** — appears in `ldd <binary>`. Must be present on the host at exactly the soname the binary requests. Currently: `libfontconfig.so.1`.
- **Runtime dlopen** — loaded via `libloading` from inside the binary at startup. Failure to resolve manifests as an application-level error rather than the loader refusing to start. Currently: `libGL.so.1`, `libEGL.so.1`, `libwayland-client.so.0`, `libwayland-egl.so.1` (probed by Slint's `i-slint-renderer-femtovg` + `winit`'s Wayland backend).

Both categories must be reflected in the per-format `depends` lists in [`crates/tisvlay-daemon/Cargo.toml`](../crates/tisvlay-daemon/Cargo.toml). The package manager only knows about declared deps; if the user installs the `.rpm` on a minimal headless distro, missing libGL/libwayland will surface at first GUI launch.

The `tisvlay` binary (the CLI plus the daemon, session, tray and greeter services) has no runtime deps beyond glibc. It shares a package with the GUI because shipping one bundle is simpler than two — if a future use case (servers, embedded) needs a daemon-only install, a `tisvlay-server` subpackage is straightforward to add.

## Why this approach (and not the obvious alternatives)

This section exists because the chosen pipeline isn't the most obvious one and we burned time trying alternatives before landing here. Documenting the rejected paths so future-us doesn't repeat the spike.

### Why not musl-static?

The most natural path on NixOS — `cargo build --target x86_64-unknown-linux-musl` produces a fully static binary with no glibc dependency, no dynamic linker, no Nix-store paths anywhere. This is what we used for the daemon-only package before tisvlay-gui existed.

It collapses for any GUI binary because **musl static binaries cannot dlopen shared libraries**. From [musl's FAQ](https://musl.libc.org/faq.html):

> Loading shared libraries from a static binary is not supported by design.

Slint's renderer dlopens libGL/libEGL via [`libloading`](https://crates.io/crates/libloading); winit dlopens libwayland-client. Both silently no-op when called from a musl-static binary — `dlopen()` returns NULL without ever invoking the loader. The first user-visible failure is `winit` reporting "the wayland library could not be loaded" even though the host has libwayland-client installed.

Even working around the *link-time* fontconfig dependency (e.g., `fontique`'s `fontconfig-dlopen` feature) doesn't help: fontconfig would then fail at runtime in the same silent way — degraded font matching rather than a missing window, but still broken. The musl-static path is fundamentally incompatible with any Rust GUI library that uses libloading at runtime.

### Why not glibc-static?

`-C target-feature=+crt-static` against `x86_64-unknown-linux-gnu` produces a static binary linked against glibc. **Glibc explicitly forbids this for any code that uses `dlopen`, NSS, locale data, or iconv** ([glibc docs](https://sourceware.org/glibc/wiki/Tips_and_Tricks/static-linking)). We use all four. Even where it "works", glibc-static binaries become bricks the moment any transitive dep calls `getaddrinfo`, `gethostbyname`, or any locale-aware function.

### Why not native `cargo build` from the Nix dev shell + `patchelf`?

A glibc-dynamic build from the dev shell hard-codes Nix-store paths into the ELF interpreter (`/nix/store/<hash>-glibc-2.42-XX/lib/ld-linux-x86-64.so.2`). On Fedora/Debian/Ubuntu, that path doesn't exist and the kernel refuses to start the binary with `No such file or directory` (ENOENT on the interpreter) before any Rust code runs. `patchelf` can rewrite the interpreter to `/lib64/ld-linux-x86-64.so.2`, but the binary's `NEEDED` entries still reference `libc.so.6` symbols at the version the Nix-shell glibc had — typically newer than what the target distro provides — so `GLIBC_2.41 not found` errors appear as soon as any libc function is called. cargo-zigbuild solves both issues at once.

### Why not `cross-rs`?

[cross-rs](https://github.com/cross-rs/cross) shells out to Docker with prebuilt cross-compilation images. It works, but:

- Requires a working Docker/Podman in the dev shell. Rootless container UX on NixOS is rough enough that we'd be fighting the tool more than using it.
- Glibc baseline is determined by the chosen base image, not a `--target` suffix. Switching glibc versions means swapping images.
- Cross's own modern glibc-baseline support [was added by integrating zig](https://github.com/cross-rs/cross/pull/1316) — using cross would mean using zigbuild indirectly, with the additional Docker layer on top. Skipping the indirection.

### Why not `manylinux`-style "build in an old container"?

Build inside an Ubuntu 20.04 / RHEL 8 container so the resulting binary's glibc symbol versions are old enough to run on newer distros. This is what [`manylinux_2_28`](https://github.com/pypa/manylinux) does for Python wheels. It's a perfectly valid approach with one tradeoff: the build environment becomes the manifest, and reproducing it requires Docker + the right base image. cargo-zigbuild lets the same dev shell that runs `cargo test` also produce shippable binaries, with the glibc target as an explicit `--target` argument rather than implicit container state.

If cargo-zigbuild ever produces a binary that misbehaves on the old end of our support window, a manylinux-style CI job is the natural fallback (see [Future work](#future-work)).

### Why not Tauri's bundler?

Tauri's Linux packaging story is "build on the oldest distro you intend to support, in a container, via GitHub Actions." It does not solve the glibc portability problem — it punts on it. Tauri also bundles webkit2gtk linkage problems on top, which we don't need.

### Why not AppImage / Flatpak?

Complementary, not replacement. AppImage in particular is a strong "long-tail compatibility" escape hatch for users on weird distros (NixOS users wanting to run a non-Nix build, distros without our declared deps, etc.). Filed as [Future work](#future-work).

## Build mechanics

Everything dispatches through [`justfile`](../justfile):

```bash
just package-build   # private — invoked by the per-format recipes
just package-deb     # produces target/<arch>-unknown-linux-gnu/dist/tisvlay_*.deb
just package-rpm     # produces target/<arch>-unknown-linux-gnu/dist/tisvlay-*.rpm
just package-archlinux
just package-all     # all three
```

The packages build with the `dist` Cargo profile, not `release`: thin LTO, one codegen unit, `panic = "abort"` and `opt-level = "s"`, which roughly halves the binaries. Its comment in the root [`Cargo.toml`](../Cargo.toml) has the measurements. Only CI builds packages, so it can afford the slower build that `release` avoids for people compiling locally.

**Architectures.** The recipes package the host's architecture (`just`'s `arch()`), so the release workflow builds x86_64 on `ubuntu-latest` and arm64 on `ubuntu-26.04-arm`, each natively, and collects both into one draft release. `ARCH=aarch64 just package-deb` cross-builds from an x86_64 host. That needs the `aarch64-unknown-linux-gnu` Rust target, and an arm64 sysroot with fontconfig for the GUI's link, so in practice arm64 packages come from CI. `package-build` points `target/nfpm-dist` at the built triple's `dist/`: nfpm doesn't expand variables in `src` paths, and the GUI binary's path in the nfpm contents goes through that link.

The dev shell ([`flake.nix`](../flake.nix) `devShells.default`) provides:

- `cargo-zigbuild` + `zig` — the build pipeline
- `nfpm` — the package generator
- `just` — recipe runner
- All the build-time C deps (`fontconfig`, `libxkbcommon`, etc.) for the GUI link step

The dev shell provides `cargo-nfpm` (the Cargo plugin that reads `[package.metadata.nfpm]` from `Cargo.toml` and shells out to `nfpm`), pinned to its crates.io release. Outside the dev shell, install it with:

```bash
cargo install cargo-nfpm
```

If `cargo nfpm` ever fails with `No such file or directory (os error 2)` on a NixOS host, the cause is almost always the orphaned-interpreter problem: cargo-installed binaries hard-code their dynamic linker into a Nix-store path, and that path gets garbage-collected when the dev shell's glibc derivation rotates. Fix:

```bash
cargo install --force cargo-nfpm
```

## Shell completions

The `tisvlay` CLI ships tab-completion for bash, zsh, and fish, present after install with no user setup. We use clap_complete's *dynamic* engine rather than static generators: the file dropped into each shell's autoload dir is a small bootstrap that calls back into the `tisvlay` binary at `<TAB>`, so completion covers subcommands, flags, and **live values** — TV UUIDs, profile UUIDs, and vendor ids, queried from the daemon and described by name — the same model Docker uses for container-name completion.

- **Generation.** `just completions` (a prerequisite of every `package-*`) runs the freshly built CLI with `COMPLETE=<shell>` set and captures the registration script into `packaging/completions/` (gitignored). It builds the CLI host-native — the zigbuild release artifact targets a foreign glibc and won't run on the NixOS build host, and the registration is a shell-name-only template so the debug build's output is identical to release. The Nix package regenerates the same scripts from `$out/bin/tisvlay` via `installShellCompletion`.
- **Install paths** (nfpm `contents`): bash → `/usr/share/bash-completion/completions/tisvlay`, zsh → `/usr/share/zsh/site-functions/_tisvlay`, fish → `/usr/share/fish/vendor_completions.d/tisvlay.fish`.
- **Zero-config caveats.** Bash needs the `bash-completion` loader present — the `.deb`/`.rpm` `recommends` it (apt installs Recommends by default; Arch/minimal/container images may not, and it is not a hard dep). Zsh needs `compinit` run (normal for interactive zsh). Fish autoloads with nothing extra. On NixOS the shell's own completion must be enabled (`programs.bash.completion.enable`, `programs.zsh.enable`, `programs.fish.enable`).
- **Manual install** on an unsupported distro: `COMPLETE=bash tisvlay > ~/.local/share/bash-completion/completions/tisvlay` (and the zsh/fish analogs). The `<TAB>` callback resolves a bare `tisvlay` on `$PATH`.
- **Live-value scope.** Completing TV/profile UUIDs and vendor ids queries the daemon per `<TAB>`, bounded by a short timeout and empty on failure so a downed daemon never hangs the prompt. Because the completer runs before the CLI parses argv, it can't see `--socket`/`--site`/`--user-socket` and always targets the default sockets + the `main` site (see [docs/ideas/completion-followups.md](../docs/ideas/completion-followups.md) for lifting that).

## Socket access & the ACL integration

The daemon's zenoh socket (`/run/tisvlay/zenoh.sock`, `0660 tisvlay:tisvlay`, dir `0750`) is the authorization boundary for everything — any process that can `connect()` has the full RPC surface. Packages integrate a **three-layer access model** (the socket-access-unification work, 2026-07; the full design record lives in the git history of `docs/plans/socket-access-unification.md`):

| Layer | Who it serves | Mechanism | Lifetime |
|---|---|---|---|
| **Durable** | SSH, remote, headless, lingering services | `tisvlay` group membership; `tisvlay enable-access` is the one-click enrollment (`pkexec usermod` through the desktop's polkit agent) | permanent — but takes effect only on a fresh login (see Lessons) |
| **Presence** | the human at the console | daemon-applied `u:<uid>:rw` ACL following logind's per-seat `ActiveSession` (`--seat-access`, default on) | while that session is the seat foreground |
| **Greeter** | the login screen's idle client (OLED protection) | daemon-applied `g:<greeter-group>:rw` ACL (`--greeter-access` auto-detect, or `--greeter-group <name>`) | while the daemon runs |

### Why the postinstall doesn't `usermod` anyone

Auto-enrolling "the user" at install time is an industry-wide refusal, not an oversight: `$SUDO_USER` is unset under PackageKit / GNOME Software (the scriptlet runs as plain root), headless installs have no human, multi-user machines have no single "the" user, and no distro policy blesses it (wireshark-common creates its group and explicitly adds no one). So the postinstall only creates the `tisvlay` system user/group; enrollment belongs to the *clients* (the CLI verb and the GUI's access banner), and the console needs none at all — the presence layer covers it. The familiar "docker group = root" critique does not transfer: that requires a root daemon, and `tisvlay-daemon` runs as an unprivileged system user, so socket access confers TV control only.

### The two ACL grants (daemon-applied, in-process)

`crates/tisvlay-daemon/src/acl.rs` writes the `system.posix_acl_access` xattr directly — no libacl dependency. A plain access ACL is a small stable blob, and *removing* the xattr reverts a file to its mode bits, which is exactly what keeps grants revocable on a runtime dir that `RuntimeDirectoryPreserve=restart` carries across daemon restarts. Load-bearing semantics, each learned the hard way:

- **Apply after the `0660` chmod, never chmod after.** A chmod recomputes the ACL mask and would clamp every named entry.
- **ACL evaluation is first-match-by-class** (owner → named users → owning + named groups → other), *not* a union: a named entry decides its principal alone. Every named entry therefore carries the full class permission (`rw` on the socket, `r-x` on the dir) — anything narrower would *subtract* access from a user who is also a group member. The mask carries the same permission so the owning `tisvlay` group keeps effective access.
- **Mode and ACL are two views of one state**: setting the ACL rewrites the file's mode bits, and the desired blob is derived from a fresh `stat` — never assumed.
- **Writes are compare-before-write**: reconcile reads the stored xattr and rewrites only on drift, so the seat watch can re-ground on a timer (every 5 minutes, wall-clock — property events can't reset it) to repair a failed or externally-clobbered write without churn or log spam.

**The greeter grant** exists because the modern GDM greeter (GNOME 49+: Fedora 43/44, Ubuntu 25.10+) is a systemd-userdb *dynamic* user whose primary group is `gdm` — `/etc/group` supplementary membership can't reach it, so `usermod -aG tisvlay gdm` targets the wrong account entirely. The group ACL keys on the one handle stable across static (NixOS) and dynamic (Fedora/Ubuntu) greeters, matching how GDM's own polkit policies gate the greeter. `--greeter-access` auto-detects `gdm`/`Debian-gdm`; a plasma-login greeter needs an explicit `--greeter-group plasmalogin` (today only the NixOS module wires that — plasma-login packaging on other distros would need its own value). Full war story and validation record: [docs/concerns/greeter-packaged-socket-access.md](../docs/concerns/greeter-packaged-socket-access.md).

**The presence grant** is `uaccess` semantics applied to our socket — logind's own mechanism for granting the active-seat user device access, which we must implement in-daemon because `uaccess` itself is udev/device-node-only. The watch (`crates/tisvlay-daemon/src/seat.rs`) keys off each seat's `ActiveSession` property — never a scan of sessions with `Active=yes`, because seatless sessions self-report active (a plain SSH shell reads `Remote=yes Type=tty Active=yes`) — and the active session qualifies only as a human desktop login: `Class=user` (excludes the greeter session) ∧ `Type∈{x11,wayland}` (excludes a bare VT) ∧ `¬Remote`. Transience is the point, not a defect: switching away removes the entry, so new connects are denied exactly while active-session arbitration suppresses that user's automation; connections that already exist persist, because the kernel checks permission at `connect()` — the same property `uaccess` accepts for device fds.

### Per-channel wiring

| Knob | `.deb` / `.rpm` / `.pkg.tar.zst` | NixOS module |
|---|---|---|
| Durable group | postinstall creates the `tisvlay` system user + group | `services.tisvlay.users` (declarative membership) |
| Greeter ACL | packaged unit's `ExecStart` passes `--greeter-access` (auto-detect) | module passes `--greeter-group` per active DM (`gdm` / `plasmalogin`), gated on `greeter.enable` |
| Presence ACL | binary defaults on — no flag in the unit; disable via a drop-in adding `--seat-access=false` | `services.tisvlay.seatAccess` (default `true`; `false` emits the opt-out flag) |

### Maintainer-script lifecycle

One shared script pair (nfpm embeds [`scripts/postinstall.sh`](scripts/postinstall.sh) / [`scripts/preremove.sh`](scripts/preremove.sh) into all three formats), but the managers pass **three different `$1` conventions**:

| Manager | remove-time script | upgrade indicator |
|---|---|---|
| rpm | `%preun` gets an instance count: `0` = final removal, `≥1` = upgrade/reinstall; `%post` gets `1` = fresh, `≥2` = upgrade | numeric |
| dpkg | `prerm` gets `remove`/`upgrade`/…; `postinst` gets `configure` with the old version in `$2` (empty on fresh install) | strings |
| pacman | `pre_remove` runs **only** on real removal and gets the old version; `post_install` gets the new version, while `post_upgrade` gets new + old versions (`$2` is set) | version strings |

Two rules follow, both burned in by field failures:

- **Teardown guards by *excluding upgrades*** (`1 | upgrade | failed-upgrade | deconfigure` → skip), never by allow-listing removal values — pacman's version-string `$1` would never match an allow-list and Arch would silently skip cleanup, leaving the daemon running after `pacman -R`. On upgrade, the *old* package's preremove is what runs — so a host upgrading from a pre-guard package will still see one final stop/disable (the new postinstall re-enables and, on upgrade, `try-restart`s; a one-time hiccup that can't be fixed retroactively).
- **Start only on genuinely fresh installs.** `enable` + `try-restart` run always (the restart swaps a *running* daemon onto the new binary; its no-op on an inactive unit is what respects an admin's deliberate stop across upgrades). The follow-up `start` fires for deb `configure` with an empty `$2` — through `deb-systemd-invoke` when present, so `policy-rc.d` is honored in chroots and image builds — rpm `$1 = 1`, or pacman with a version-valued `$1` and empty `$2` (fresh `post_install`; `post_upgrade` supplies the old version in `$2`). RPM's numeric upgrade count does not match the pacman version check.

**Log out and back in after upgrading.** The postinstall restarts only the system daemon; each logged-in user's session, tray and GUI keep running the old binaries until re-login. The rpc wire has no cross-version compatibility (every client ships with the daemon), so in that window an old user service and the new daemon may refuse each other's requests.

One known benign leftover: packages older than the ACL model added the static `gdm` user to the `tisvlay` group; nothing revokes that on upgrade, so `getent group tisvlay` may still list `gdm` on long-lived hosts. Harmless — the static `gdm` user is not the greeter on modern GDM, and the trusted-DM posture is the same one the greeter ACL grants deliberately.

### Verifying on a live host

```bash
sudo getfacl -p /run/tisvlay/zenoh.sock   # user:<name>:rw- while seated; group:gdm:rw-; mask::rw-
journalctl -u tisvlay-daemon | grep "socket ACL"   # "socket ACL applied" on real transitions only
tisvlay status            # from the console: works with no group membership
tisvlay enable-access     # grants the durable group (gates on membership, not reachability)
```

Field-validated on Fedora 44 and Ubuntu 26.04 (both GNOME/GDM 50 with dynamic userdb greeters): console login mints the `user:` entry beside `group:gdm`; at the greeter there is no user entry (the `Class` gate); seatless SSH without the group gets the CLI's `enable-access` hint; full `remove` → `install` cycles are clean in both directions.

### Lessons

- **A group grant does not take effect on logout/login.** The systemd per-user manager (`user@<uid>.service`) outlives a logout and keeps its supplementary-group set frozen at first login, so the re-login re-attaches to the stale manager. A reboot (or `loginctl terminate-user`) is the reliable path — the enroll-success copy in the CLI and GUI says exactly that. The presence ACL is immune: it attaches to the *socket*, not to any process's credentials, so it works the moment it lands.
- **A printed instruction in a scriptlet reaches no one under an app-store install.** Twice bitten: the old "now run `usermod`" hint (replaced by `enable-access` + the presence layer) and the old "now start the daemon" hint (replaced by the fresh-install start above).
- **EACCES poisons path resolution before it ever reaches `connect()`.** A user locked out of `/run/tisvlay/` can't even `stat` the socket, so `Path::exists()` reads EACCES as absent and the CLI's connect-side resolution falls back to the (absent) dev socket — producing a misleading "daemon unreachable at `/run/user/…`" error for what is really an access problem. The CLI's no-access hint and the clients' nonblocking connect-probe classification (`EACCES` ⇒ no access; `ECONNREFUSED` ⇒ daemon down) exist because of this.

## Hand-editing the daemon's store

The daemon keeps its TVs (credentials included) and system profiles in `/var/lib/tisvlay/daemon.toml`, and applies a hand edit on reload: the daemon treats SIGHUP as its reload rpc, and the units' `ExecReload` sends it, so `systemctl reload tisvlay-daemon` works on every channel. How a reload merges, refuses and preserves an edit is user-facing behavior, documented once in the user guide: [Configuration → Hand-editing and reloading](../docs/book/src/guide/configuration.md#hand-editing-and-reloading).

## Testing the result

The build host produces the package, but only the target distro proves it installs and runs.

### Quick local smoke test (Docker / Podman)

The [`docker/Dockerfile`](docker/Dockerfile) provides a clean Ubuntu 24.04 environment that mirrors the GitHub Actions `ubuntu-latest` runner:

```bash
docker build -t tisvlay-ci -f packaging/docker/Dockerfile .
docker run --rm -v "$PWD:/work" -w /work tisvlay-ci just clippy
```

That validates the GUI compiles against a real distro's libfontconfig headers. Add an analogous Fedora-based image when needed for `.rpm` runtime testing — `dnf install ./tisvlay-*.rpm` followed by `tisvlay-gui --version` is enough to catch missing runtime deps.

### Real-distro VM smoke test

The actual ground-truth test: copy the produced `.rpm` (or `.deb`) to a VM running the target distro, install via the native package manager, run `tisvlay-gui` from a graphical session.

Verified working as of this commit:
- **Bazzite** (Fedora 42 base, KDE Plasma on Wayland, glibc 2.41): GUI launches, daemon connection over the system bus, pairing wizard interactive.

Known benign noise during VM testing:

- `libEGL warning: egl: failed to create dri2 screen` (printed twice). Mesa probes the DRI2 fast path during EGL init and falls back to whatever rendering path the VM's virtio-gpu / qxl driver actually exposes. The fallback works — interactive GUI rendering proves it. Disappears on bare metal with a real GPU.

## Third-party FOSS attribution

Each `.deb` / `.rpm` / `.pkg.tar.zst` ships two attribution files at `/usr/share/doc/tisvlay/`:

- `THIRD-PARTY-LICENSES.html` — workspace-wide bundled licenses for every transitively-used crate. Regenerated at the source root before each build by `just licenses-html` (a prerequisite of every `package-*` recipe), gitignored.
- `NOTICE` — hand-written project copyright + Apache-2.0 election.

The Nix package mirrors the same paths under `$out/share/doc/tisvlay/` via `postInstall` in [`nix/package.nix`](nix/package.nix).

For developer-facing details — the cargo-about + cargo-deny pipeline, per-crate exception catalog, license elections, how to add an exception when a dep bump fails the policy gate — see [`docs/licensing.md`](../docs/licensing.md).

## Known issues / caveats

- **`libfontconfig.so.1` is a hard runtime dep through Slint 1.x.** Direct linkage is intentional upstream policy as of Slint 1.15: maintainers reverted the `fontconfig-dlopen` path that briefly shipped in 1.14.x because dlopen-mode silently breaks CJK/emoji fallback when libfontconfig isn't present at startup ([slint #10295](https://github.com/slint-ui/slint/issues/10295), [parley #464](https://github.com/linebender/parley/issues/464), 2026-01-30). The maintainer position: surface failures at link/load time rather than as silent text-rendering bugs. Our per-format `depends` lists pull libfontconfig in, so this is benign — but flipping `RUST_FONTCONFIG_DLOPEN=on` (or patching `i-slint-common`'s `fontconfig-dlopen` feature) would re-introduce the silent-fallback bug upstream just fixed. Don't. The pure-Rust replacement is tracked at [slint #3906](https://github.com/slint-ui/slint/issues/3906) — open, no priority label, no roadmap entry; realistic horizon >12 months. Plan around fontconfig staying for the foreseeable future.
- **The `-sys` crate `cc` mismatch hazard**: vendored C deps (`libpng-sys`, `freetype-sys`, etc., when transitively compiled from source) call a `cc` from `build.rs`. Without explicit `CC_x86_64_unknown_linux_gnu` and `CXX_*` env vars pointing at `zig cc -target x86_64-linux-gnu.<baseline>`, those C objects can pick up the dev shell's host glibc symbols rather than the zig-shimmed baseline. Currently no transitive C deps in our graph fall into this trap; if future deps do, see [Future work](#future-work).
- **`cargo-nfpm` orphaned interpreter**: documented above — `cargo install --force cargo-nfpm` fixes it.
- **Per-packager `depends`/`recommends` must nest under `overrides.<packager>`.** cargo-nfpm's `[package.metadata.nfpm.deb]` / `[nfpm.rpm]` tables (its `DebSpecificSettings`/`RpmSpecificSettings`) have **no** `depends`/`recommends` field, and nothing rejects unknown keys — so deps declared directly there are silently dropped and the built package ends up with an empty `Depends`. The runtime libs live under `[package.metadata.nfpm.overrides.deb]` / `[nfpm.overrides.rpm]` (nfpm's `overrides.<packager>`, applied per `--packager`). Verify with `dpkg-deb -f <deb> Depends Recommends` after a `depends` edit.
- **Bazzite (`rpm-ostree`-based) install path**: Bazzite uses `rpm-ostree` rather than plain `dnf`. `dnf install ./tisvlay-*.rpm` will not work on Bazzite atomic; use `rpm-ostree install ./tisvlay-*.rpm` (and reboot) or `rpm-ostree install --apply-live` for in-place layering.

## Future work

- **Track [slint #3906](https://github.com/slint-ui/slint/issues/3906)** (replace fontconfig with a pure-Rust alternative). Currently the only realistic path to dropping `libfontconfig` from the runtime dep list. No priority label upstream; horizon >12 months. When it lands, the link-time fontconfig dep + the corresponding `depends` entries can come out.
- **`CC_x86_64_unknown_linux_gnu` / `CXX_*` pinned to `zig cc -target x86_64-linux-gnu.<baseline>`** in the release build env. Defends against the `-sys` `cc` mismatch above.
- **Manylinux-style fallback CI job** that builds inside `quay.io/pypa/manylinux_2_28_x86_64` (or `ubuntu:20.04`) using plain `cargo build`. Provides instant root-cause signal if zigbuild ever produces a binary that misbehaves on the old end of the support window. Not needed today; cheap to add when the support matrix matters.
- **AppImage artifact** alongside the `.deb`/`.rpm`. Roughly half a day of work via `linuxdeploy`. Covers the long tail of "I'm on Slackware / Void / NixOS" reports forever.
- **`tisvlay-server` subpackage** (daemon + CLI + session, no GUI, no GUI runtime deps). For headless installs (NAS, embedded). Straightforward to add via a second `[package.metadata.nfpm-server]`-style block once a use case materializes. Could also bring back musl-static for that subpackage since it has no dlopen dependencies.

## References

- Research that grounded the cargo-zigbuild choice (worth re-reading if reconsidering): [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), [Kobzol on portable Rust binaries](https://kobzol.github.io/rust/ci/2021/05/07/building-rust-binaries-in-ci-that-work-with-older-glibc.html), [Rust blog on increasing glibc/kernel requirements](https://blog.rust-lang.org/2022/08/01/Increasing-glibc-kernel-requirements/).
- Why musl-static doesn't work for GUI binaries: [musl FAQ on dlopen](https://musl.libc.org/faq.html), [Slint PR #956](https://github.com/slint-ui/slint/pull/956) (the upstream switch to dlopen-friendly fontconfig).
- nfpm format reference: [`nfpm` docs](https://nfpm.goreleaser.com/configuration/). Per-format keys mirror what we declare in [`crates/tisvlay-daemon/Cargo.toml`](../crates/tisvlay-daemon/Cargo.toml).
- Nix-side packaging (separate channel, separate concerns): [`nix/README.md`](nix/README.md).
