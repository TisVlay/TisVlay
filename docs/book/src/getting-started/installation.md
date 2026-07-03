# Installation

tisvlay ships as pre-built distro packages, a NixOS module, and as source via Cargo. Pick the path that matches your system.

After installing, enable the services:

```sh
# System daemon
sudo systemctl enable --now tisvlayd
# Per-user desktop services (run as your normal user, not root)
systemctl --user enable --now tisvlay-session tisvlay-tray
```

The daemon runs as a dedicated `tisvlay` system user and stores its state under `/var/lib/tisvlay/`.

## Distro packages (.deb / .rpm / .pkg.tar.zst)

Pre-built packages are published on the [releases page](https://github.com/ruffsl/tisvlay/releases) for each tagged version. They are statically linked, so they work across glibc versions and on immutable-root distros.

```sh
export VERSION=0.1.0
export BASE=https://github.com/ruffsl/tisvlay/releases/download/v${VERSION}
```

**Debian / Ubuntu**

```sh
wget ${BASE}/tisvlay_${VERSION}-1_amd64.deb
sudo apt install ./tisvlay_${VERSION}-1_amd64.deb
```

**Fedora / RHEL**

```sh
wget ${BASE}/tisvlay-${VERSION}-1.x86_64.rpm
sudo dnf install ./tisvlay-${VERSION}-1.x86_64.rpm
```

**Bazzite / Silverblue** (immutable; reboot required)

```sh
sudo rpm-ostree install ./tisvlay-${VERSION}-1.x86_64.rpm
```

**Arch / CachyOS**

```sh
wget ${BASE}/tisvlay-${VERSION}-1-x86_64.pkg.tar.zst
sudo pacman -U ./tisvlay-${VERSION}-1-x86_64.pkg.tar.zst
```

The post-install hook reloads systemd and the D-Bus policy automatically.

## NixOS (flake module)

The repository's flake provides a NixOS module that wires up the daemon, user services, D-Bus policy, and CLI together.

```nix
# flake.nix
{
  inputs.tisvlay.url = "github:ruffsl/tisvlay";

  outputs = { nixpkgs, tisvlay, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      modules = [
        tisvlay.nixosModules.default
        { services.tisvlay.enable = true; }
      ];
    };
  };
}
```

This enables `tisvlayd` (as the `tisvlay` user), the `tisvlay-session` and `tisvlay-tray` user services, the D-Bus policy, and the `tisvlay` CLI in `$PATH`.

## Cargo (from source)

```sh
git clone https://github.com/ruffsl/tisvlay
cd tisvlay
cargo build --release
```

Binaries land in `target/release/`. Building from Cargo does **not** install the systemd units or D-Bus policy — copy those from [`packaging/`](https://github.com/ruffsl/tisvlay/tree/main/packaging) if you want the daemon and session services managed by systemd.

A Rust toolchain is required; install it from <https://rust-lang.org/tools/install/>.

## HDMI-CEC backend (optional)

The HDMI-CEC backend needs a CEC adapter that the kernel exposes as a `/dev/cecN` character device — most GPU HDMI ports don't wire the CEC pin. Two known-working sources:

- A **USB CEC adapter** (e.g. Pulse-Eight), via the kernel `pulse8-cec` driver.
- An active **DisplayPort→HDMI adapter on an amdgpu or i915 output** — the GPU driver tunnels DP-AUX CEC. **Nvidia GPUs expose no CEC**, so use a USB adapter there.

On NixOS, enable device access for the sandboxed daemon with `services.tisvlay.cec.enable = true` (it installs the udev rule and grants the daemon `char-cec` access). The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Pairing](./pairing.md#hdmi-cec-no-pairing-needed).
