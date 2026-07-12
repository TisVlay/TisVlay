# Installation

tisvlay ships as pre-built distro packages, a NixOS module, and as source via Cargo. Pick the path that matches your system.

After installing, enable the services:

```sh
# System daemon
sudo systemctl enable --now tisvlay-daemon
# Per-user desktop services (run as your normal user, not root)
systemctl --user enable --now tisvlay-session tisvlay-tray
```

The daemon runs as a dedicated `tisvlay` system user and stores its state under `/var/lib/tisvlay/`. Access to the daemon is gated by the **`tisvlay` group** (it owns the daemon's control socket), so add your desktop user to it and re-login:

```sh
sudo usermod -aG tisvlay $USER   # re-login for the group to take effect
```

## Distro packages (.deb / .rpm / .pkg.tar.zst)

Pre-built packages are published on the [releases page](https://github.com/ruffsl/tisvlay/releases) for each tagged version. They are statically linked, so they work across glibc versions and on immutable-root distros.

```sh
export VERSION=0.3.0
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

The post-install hook reloads systemd, refreshes the desktop database, creates the `tisvlay` system user and group, and enables `tisvlay-daemon`. It does **not** add you to the `tisvlay` group — do that yourself (above) so the CLI and desktop services can reach the daemon.

## NixOS (flake module)

The repository's flake provides a NixOS module that wires up the daemon, user services, and CLI together.

```nix
# flake.nix
{
  inputs.tisvlay.url = "github:ruffsl/tisvlay";

  outputs = { nixpkgs, tisvlay, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      modules = [
        tisvlay.nixosModules.default
        {
          services.tisvlay.enable = true;
          services.tisvlay.users = [ "yourname" ];   # socket-access grant (tisvlay group)
        }
      ];
    };
  };
}
```

`services.tisvlay.enable` runs `tisvlay-daemon` (as the `tisvlay` user), the `tisvlay-session` and `tisvlay-tray` user services, and the `tisvlay` CLI in `$PATH`. **`services.tisvlay.users` is required** — a desktop user not listed there can't reach the daemon socket, so the session, tray, and GUI won't connect.

Other useful options:

- `services.tisvlay.cec.enable` — device access for the HDMI-CEC backend (see below); `services.tisvlay.cec.pulse8.enable` additionally builds the in-tree Pulse-Eight USB-CEC driver.
- `services.tisvlay.greeter.enable` — the optional login-screen idle client (defaults on when `plasma-login-manager` is the display manager).
- `services.tisvlay.openFirewall` / `services.tisvlay.bindPort` — open the pinned SSDP discovery source port (default `1901`); see [Firewall & Discovery](../operations/firewall-discovery.md).

## Cargo (from source)

```sh
git clone https://github.com/ruffsl/tisvlay
cd tisvlay
cargo build --release
```

Binaries land in `target/release/`. Building from Cargo does **not** install the systemd units — copy those from [`packaging/`](https://github.com/ruffsl/tisvlay/tree/main/packaging) if you want the daemon and session services managed by systemd, and create the `tisvlay` group yourself.

A Rust toolchain is required; install it from <https://rust-lang.org/tools/install/>.

## HDMI-CEC backend (optional)

The HDMI-CEC backend needs a CEC adapter that the kernel exposes as a `/dev/cecN` character device — most GPU HDMI ports don't wire the CEC pin. Two known-working sources:

- A **USB CEC adapter** (e.g. Pulse-Eight), via the kernel `pulse8-cec` driver.
- An active **DisplayPort→HDMI adapter on an amdgpu or i915 output** — the GPU driver tunnels DP-AUX CEC. **Nvidia GPUs expose no CEC**, so use a USB adapter there.

On NixOS, enable device access for the sandboxed daemon with `services.tisvlay.cec.enable = true` (it installs the udev rule granting the `tisvlay` group read/write on `/dev/cec*` and permits the char-cec subsystem through the daemon's hardened unit). For a Pulse-Eight USB adapter, `services.tisvlay.cec.pulse8.enable = true` also builds the in-tree driver. The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Pairing](./pairing.md#hdmi-cec-no-pairing-needed).
