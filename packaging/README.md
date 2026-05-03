# tisvlay — packaging

High-level overview of every channel we ship through, why the build pipeline is shaped the way it is, and what to know before changing it.

## Channels

| Channel | Source of truth | Use case |
|---|---|---|
| **Nix flake / NixOS module** | [`nix/package.nix`](nix/package.nix), [`nix/module.nix`](nix/module.nix) | NixOS hosts, declarative service config. See [`nix/README.md`](nix/README.md) for the full deep-dive on the Nix derivation, hardening posture, VM test, and module options. |
| **`.deb` / `.rpm` / `.pkg.tar.zst`** (this directory) | [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml) `[package.metadata.nfpm*]` + [`justfile`](../justfile) `package-*` recipes | Debian/Ubuntu, Fedora/RHEL, Arch Linux. One bundled package per format containing all five binaries plus systemd units and D-Bus policy. |
| **Test container** | [`docker/Dockerfile`](docker/Dockerfile) | Reproducible Ubuntu environment for verifying the build matrix without touching CI, and for smoke-testing the resulting `.rpm`/`.deb` against a clean glibc target. |
| **Dev-shell native build** | [`flake.nix`](../flake.nix) `devShells.default` | `cargo run` / `cargo test` / `just check` directly from the Nix dev shell. **Not** what produces shippable binaries — see "Build pipeline" below. |

The rest of this document is about the second channel — the cross-distro `.deb`/`.rpm`/`.pkg.tar.zst` produced by `just package-{deb,rpm,archlinux,all}`.

## Build pipeline

```
┌─────────────────────────────┐
│ NixOS dev shell             │
│   rust-toolchain.toml       │
│   cargo-zigbuild + zig      │   linker = zig cc
│   nfpm (Go binary)          │   target  = x86_64-unknown-linux-gnu.<glibc_baseline>
└──────────────┬──────────────┘
               │  just package-build
               ▼
┌─────────────────────────────┐
│ target/x86_64-unknown-linux │
│        -gnu/release/         │
│   tisvlayd, tisvlay,         │   ELF interpreter:
│   tisvlay-session,           │     /lib64/ld-linux-x86-64.so.2
│   tisvlay-tray, tisvlay-gui  │   glibc symbols: ≥ glibc_baseline
└──────────────┬──────────────┘
               │  cargo nfpm package -p tisvlayd ... -f deb|rpm|archlinux
               ▼
┌─────────────────────────────┐
│ tisvlay_<ver>_amd64.deb     │
│ tisvlay-<ver>.x86_64.rpm    │   Runs on any distro at or newer than
│ tisvlay-<ver>-...pkg.tar.zst │   `glibc_baseline` declared in justfile.
└─────────────────────────────┘
```

The two ingredients that make this work cleanly from a NixOS host are:

- **cargo-zigbuild** invokes `cargo` with zig as the C/C++ linker. Zig's standard library bundles glibc shim libraries for many versions; passing `--target x86_64-unknown-linux-gnu.<version>` tells it to link only symbols available at-or-before that glibc version. The ELF interpreter is the standard `/lib64/ld-linux-x86-64.so.2` (no Nix-store path), and the dynamic-section `NEEDED` entries name plain SONAMEs (`libc.so.6`, `libfontconfig.so.1`, …). The result loads on any distro with glibc ≥ baseline without further tooling.
- **nfpm** (the Go binary, invoked through the `cargo-nfpm` plugin) reads `[package.metadata.nfpm*]` from [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml) and produces a `.deb`/`.rpm`/`.pkg.tar.zst` from the same source-of-truth. Per-format `depends` lists declare runtime libraries the package manager must pull in.

### Glibc baseline

Set in [`justfile`](../justfile) as `glibc_baseline := "2.31"`. Coverage:

| baseline | covers | notes |
|---|---|---|
| 2.17 | RHEL 7+ | Very portable, very old; Zig has stability issues with such old targets. |
| 2.28 | RHEL 8+, Debian 10+, Ubuntu 18.04+ | What `manylinux_2_28` (Python wheel standard) picks. |
| **2.31** | RHEL 9+, Debian 11+, Ubuntu 20.04+, Fedora 33+ | Current pick. Comfortable middle ground; clear of Zig's known undefined-symbol bugs at older anchors. |
| 2.34 | RHEL 9+, Debian 12+, Ubuntu 22.04+, Fedora 35+ | More-permissive 2026 floor; drops nothing anyone still desktop-installs. |

Bumping the baseline is a one-line change. Going below 2.28 starts hitting [zig issue #15486](https://github.com/ziglang/zig/issues/15486), [#5882](https://github.com/ziglang/zig/issues/5882), [#9485](https://github.com/ziglang/zig/issues/9485) (undefined `__isoc23_strtoul`, `fcntl64`, `aligned_alloc`).

### Runtime dependencies

`tisvlay-gui` is the only binary that pulls non-glibc deps. Two categories:

- **Link-time (`NEEDED` in the ELF)** — appears in `ldd <binary>`. Must be present on the host at exactly the soname the binary requests. Currently: `libfontconfig.so.1`.
- **Runtime dlopen** — loaded via `libloading` from inside the binary at startup. Failure to resolve manifests as an application-level error rather than the loader refusing to start. Currently: `libGL.so.1`, `libEGL.so.1`, `libwayland-client.so.0`, `libwayland-egl.so.1`, `libxcb.so` (probed by Slint's `i-slint-renderer-femtovg` + `winit` wayland/x11 backends).

Both categories must be reflected in the per-format `depends` lists in [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml). The package manager only knows about declared deps; if the user installs the `.rpm` on a minimal headless distro, missing libGL/libwayland will surface at first GUI launch.

The daemon, CLI, session, and tray binaries have no runtime deps beyond glibc. They ride along in the same package because shipping one bundle is simpler than four — if a future use case (servers, embedded) needs a daemon-only install, a `tisvlay-server` subpackage is straightforward to add.

## Why this approach (and not the obvious alternatives)

This section exists because the chosen pipeline isn't the most obvious one and we burned time trying alternatives before landing here. Documenting the rejected paths so future-us doesn't repeat the spike.

### Why not musl-static?

The most natural path on NixOS — `cargo build --target x86_64-unknown-linux-musl` produces a fully static binary with no glibc dependency, no dynamic linker, no Nix-store paths anywhere. This is what we used for the daemon-only package before tisvlay-gui existed.

It collapses for any GUI binary because **musl static binaries cannot dlopen shared libraries**. From [musl's FAQ](https://musl.libc.org/faq.html):

> Loading shared libraries from a static binary is not supported by design.

Slint's renderer dlopens libGL/libEGL via [`libloading`](https://crates.io/crates/libloading); winit dlopens libwayland-client; libxcb is dlopened as a fallback. All of these silently no-op when called from a musl-static binary — `dlopen()` returns NULL without ever invoking the loader. The first user-visible failure is `winit` reporting "the wayland library could not be loaded" even though the host has libwayland-client installed.

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
just package-deb     # produces target/x86_64-unknown-linux-gnu/release/tisvlay_*.deb
just package-rpm     # produces target/x86_64-unknown-linux-gnu/release/tisvlay-*.rpm
just package-archlinux
just package-all     # all three
```

The dev shell ([`flake.nix`](../flake.nix) `devShells.default`) provides:

- `cargo-zigbuild` + `zig` — the build pipeline
- `nfpm` — the package generator
- `just` — recipe runner
- All the build-time C deps (`fontconfig`, `libxkbcommon`, etc.) for the GUI link step

`cargo-nfpm` (the Cargo plugin that reads `[package.metadata.nfpm]` from `Cargo.toml` and shells out to `nfpm`) is **not** in nixpkgs and must be installed separately:

```bash
cargo install cargo-nfpm
```

If `cargo nfpm` ever fails with `No such file or directory (os error 2)` on a NixOS host, the cause is almost always the orphaned-interpreter problem: cargo-installed binaries hard-code their dynamic linker into a Nix-store path, and that path gets garbage-collected when the dev shell's glibc derivation rotates. Fix:

```bash
cargo install --force cargo-nfpm
```

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

## Known issues / caveats

- **`yeslogic-fontconfig-sys` link**: fontconfig is currently linked at build time. This works because every reasonable Linux desktop has libfontconfig installed and our `depends` list pulls it in. If a future user reports symbol-version errors against `libfontconfig.so.1`, the fix is to set `RUST_FONTCONFIG_DLOPEN=on` at build time (see [Future work](#future-work)) — that switches fontconfig to runtime-loaded via libloading and removes the link-time dep entirely.
- **The `-sys` crate `cc` mismatch hazard**: vendored C deps (`libpng-sys`, `freetype-sys`, etc., when transitively compiled from source) call a `cc` from `build.rs`. Without explicit `CC_x86_64_unknown_linux_gnu` and `CXX_*` env vars pointing at `zig cc -target x86_64-linux-gnu.<baseline>`, those C objects can pick up the dev shell's host glibc symbols rather than the zig-shimmed baseline. Currently no transitive C deps in our graph fall into this trap; if future deps do, see [Future work](#future-work).
- **`cargo-nfpm` orphaned interpreter**: documented above — `cargo install --force cargo-nfpm` fixes it.
- **Bazzite (`rpm-ostree`-based) install path**: Bazzite uses `rpm-ostree` rather than plain `dnf`. `dnf install ./tisvlay-*.rpm` will not work on Bazzite atomic; use `rpm-ostree install ./tisvlay-*.rpm` (and reboot) or `rpm-ostree install --apply-live` for in-place layering.

## Future work

- **`RUST_FONTCONFIG_DLOPEN=on`** in the release build env. Removes the only link-time non-glibc dep, simplifies the `depends` list (libfontconfig moves from declared dep to "best-effort, loaded at runtime if present"), pre-empts a class of symbol-version failures. Cheap; do it before the first non-test release.
- **`CC_x86_64_unknown_linux_gnu` / `CXX_*` pinned to `zig cc -target x86_64-linux-gnu.<baseline>`** for the same env. Defends against the `-sys` `cc` mismatch above.
- **Manylinux-style fallback CI job** that builds inside `quay.io/pypa/manylinux_2_28_x86_64` (or `ubuntu:20.04`) using plain `cargo build`. Provides instant root-cause signal if zigbuild ever produces a binary that misbehaves on the old end of the support window. Not needed today; cheap to add when the support matrix matters.
- **AppImage artifact** alongside the `.deb`/`.rpm`. Roughly half a day of work via `linuxdeploy`. Covers the long tail of "I'm on Slackware / Void / NixOS" reports forever.
- **`tisvlay-server` subpackage** (daemon + CLI + session, no GUI, no GUI runtime deps). For headless installs (NAS, embedded). Straightforward to add via a second `[package.metadata.nfpm-server]`-style block once a use case materializes. Could also bring back musl-static for that subpackage since it has no dlopen dependencies.

## References

- Research that grounded the cargo-zigbuild choice (worth re-reading if reconsidering): [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), [Kobzol on portable Rust binaries](https://kobzol.github.io/rust/ci/2021/05/07/building-rust-binaries-in-ci-that-work-with-older-glibc.html), [Rust blog on increasing glibc/kernel requirements](https://blog.rust-lang.org/2022/08/01/Increasing-glibc-kernel-requirements/).
- Why musl-static doesn't work for GUI binaries: [musl FAQ on dlopen](https://musl.libc.org/faq.html), [Slint PR #956](https://github.com/slint-ui/slint/pull/956) (the upstream switch to dlopen-friendly fontconfig).
- nfpm format reference: [`nfpm` docs](https://nfpm.goreleaser.com/configuration/). Per-format keys mirror what we declare in [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml).
- Nix-side packaging (separate channel, separate concerns): [`nix/README.md`](nix/README.md).
