# Plan: Multi-distro packaging and crates.io publishing

## Motivation

The project currently only packages for NixOS (`packaging/nix/`). To
reach a wider audience, we need distro packages (.deb, .rpm, .pkg.tar.zst)
and crates.io publishing.

## Design decisions

### crates.io and distro packaging are separate concerns

**crates.io** publishes individual Rust crates. Users install specific
binaries:

```bash
cargo install tisvlay-cli      # CLI tool
cargo install tisvlayd          # daemon (no service files)
```

`cargo install` places binaries in `~/.cargo/bin/`. It does not install
systemd service files, D-Bus policies, or create system users. This is
inherent to `cargo install` and is how all Rust daemon projects handle
it (spotifyd, kanidm, greetd all have this limitation).

**Distro packages** (.deb, .rpm, etc.) bundle all binaries + service
files into a single installable package named `tisvlay`. The package
manager handles systemd setup, D-Bus policy installation, user creation,
and file placement in `/usr/bin/`.

### No meta crate

Research shows no real-world Rust projects use a "meta" or "umbrella"
crate pattern. Projects with multiple binaries (Nushell, Kanidm, Gitoxide)
publish each crate independently. The distro package name (`tisvlay`) is
independent of any crate name.

### cargo-nfpm for distro packages

`cargo-nfpm` generates .deb, .rpm, .apk, .pkg.tar.zst, and .ipk from
a single `[package.metadata.nfpm]` config in Cargo.toml. It wraps
nfpm (by GoReleaser) with Cargo integration:

- Reads metadata from Cargo.toml (no separate config files)
- Auto-downloads nfpm binary (vendored)
- Supports workspaces via `-p` flag
- One command per format: `cargo nfpm package -p tisvlayd -f deb`

The nfpm config lives on `tisvlayd`'s Cargo.toml because the daemon
is the primary deliverable and the packaging anchor. The `-p tisvlayd`
flag tells cargo-nfpm where to read the config — the output package
name is set inside the config (`name = "tisvlay"`).

## Package contents

The `tisvlay` distro package includes:

| File | Destination | Notes |
|------|-------------|-------|
| `tisvlayd` | `/usr/bin/tisvlayd` | System daemon |
| `tisvlay` | `/usr/bin/tisvlay` | CLI tool |
| `tisvlay-session` | `/usr/bin/tisvlay-session` | User session service |
| `tisvlay-tray` | `/usr/bin/tisvlay-tray` | System tray app |
| `tisvlayd.service` | `/usr/lib/systemd/system/` | System service unit |
| `tisvlay-session.service` | `/usr/lib/systemd/user/` | User service unit |
| `tisvlay-tray.service` | `/usr/lib/systemd/user/` | User service unit |
| `org.tisvlay.Daemon.service` | `/usr/share/dbus-1/system-services/` | D-Bus activation |
| `org.tisvlay.Daemon.conf` | `/etc/dbus-1/system.d/` | D-Bus policy |

## crates.io publishing

Publish individual crates in dependency order:

1. `tisvlay-core` — core traits, types, D-Bus client
2. `tisvlay-lg` — LG WebOS backend
3. `tisvlay-backends` — backend registry
4. `tisvlayd` — system daemon
5. `tisvlay-cli` — CLI tool
6. `tisvlay-session` — session service
7. `tisvlay-tray` — tray app

Each crate needs in its `Cargo.toml`:
- `description` — required by crates.io
- `license` — required by crates.io
- `repository`, `homepage` — recommended
- `categories`, `keywords` — for discoverability

The workspace already has most of these via `[workspace.package]`.

## Implementation plan

### Phase 1: cargo-nfpm config

Add `[package.metadata.nfpm]` to `crates/tisvlayd/Cargo.toml`:

```toml
[package.metadata.nfpm]
name = "tisvlay"
description = "Multi-vendor TV control for Linux"
license = "Apache-2.0"
homepage = "https://github.com/ruffsl/tisvlay"
contents = [
    # Binaries
    { src = "target/release/tisvlayd", dst = "/usr/bin/tisvlayd" },
    { src = "target/release/tisvlay", dst = "/usr/bin/tisvlay" },
    { src = "target/release/tisvlay-session", dst = "/usr/bin/tisvlay-session" },
    { src = "target/release/tisvlay-tray", dst = "/usr/bin/tisvlay-tray" },
    # Systemd services
    { src = "systemd/tisvlayd.service", dst = "/usr/lib/systemd/system/tisvlayd.service" },
    { src = "systemd/tisvlay-session.service", dst = "/usr/lib/systemd/user/tisvlay-session.service" },
    { src = "systemd/tisvlay-tray.service", dst = "/usr/lib/systemd/user/tisvlay-tray.service" },
    # D-Bus
    { src = "systemd/org.tisvlay.Daemon.service", dst = "/usr/share/dbus-1/system-services/org.tisvlay.Daemon.service" },
    { src = "systemd/org.tisvlay.Daemon.conf", dst = "/etc/dbus-1/system.d/org.tisvlay.Daemon.conf" },
]

[package.metadata.nfpm.scripts]
postinstall = "packaging/scripts/postinstall.sh"
preremove = "packaging/scripts/preremove.sh"

[package.metadata.nfpm.deb]
depends = ["libc6"]

[package.metadata.nfpm.rpm]
depends = ["glibc"]
```

### Phase 2: Install scripts

Create `packaging/scripts/`:

- `postinstall.sh` — create `tisvlay` system user, reload systemd +
  D-Bus, enable `tisvlayd.service`
- `preremove.sh` — stop and disable services before uninstall

### Phase 3: Build and test

```bash
# Build all binaries
cargo build --release --workspace

# Generate packages (requires cargo-nfpm installed)
cargo nfpm package -p tisvlayd --no-build -f deb
cargo nfpm package -p tisvlayd --no-build -f rpm
cargo nfpm package -p tisvlayd --no-build -f archlinux
```

Test on target distros (VM or container).

### Phase 4: GitHub Actions release workflow

CI matrix that builds all formats on tag push:

```yaml
on:
  push:
    tags: ['v*']

jobs:
  release:
    steps:
      - cargo build --release --workspace
      - cargo nfpm package -p tisvlayd --no-build -f deb
      - cargo nfpm package -p tisvlayd --no-build -f rpm
      - cargo nfpm package -p tisvlayd --no-build -f archlinux
      - upload to GitHub release assets
```

### Phase 5: crates.io publishing

Ensure all workspace crates have required metadata, then:

```bash
cargo publish -p tisvlay-core
cargo publish -p tisvlay-lg
cargo publish -p tisvlay-backends
cargo publish -p tisvlayd
cargo publish -p tisvlay-cli
cargo publish -p tisvlay-session
cargo publish -p tisvlay-tray
```

Document in README that `cargo install` provides binaries only —
systemd setup requires the distro package or manual installation
of service files from the repo.

---

## Checklist

- [ ] Phase 1: cargo-nfpm config on tisvlayd
- [ ] Phase 2: postinstall/preremove scripts
- [ ] Phase 3: Build and test packages
- [ ] Phase 4: GitHub Actions release workflow
- [ ] Phase 5: crates.io publishing prep
- [ ] Update design.md and CLAUDE.md

---

## References

- [cargo-nfpm](https://github.com/RagnarLab/cargo-nfpm) — Cargo plugin
  for nfpm, generates deb/rpm/archlinux/apk/ipk from Cargo.toml
- [nfpm](https://nfpm.goreleaser.com/) — cross-distro package builder
- [Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
- Real-world patterns: spotifyd (daemon, no service in crate), kanidm
  (server + client published separately), greetd (distro packages only)
