# Installation

Most Linux users can install a pre-built package below. Choose the instructions for your distribution.

## Distro packages (.deb / .rpm / .pkg.tar.zst / flake)

Pre-built packages are published on the [releases page](https://github.com/TisVlay/TisVlay/releases) for each tagged version, for x86_64 and arm64 (aarch64). The commands below pick the one that matches your machine. Each package is signed; to check one before you install it, see [Security & Supply Chain](../reference/security-supply-chain.md).

```sh
{{#include ../generated/release-version.txt}}
export BASE=https://github.com/TisVlay/TisVlay/releases/download/v${VERSION}
```

### Debian / Ubuntu

```sh
ARCH=$(dpkg --print-architecture)   # amd64 or arm64
wget ${BASE}/tisvlay_${VERSION}-1_${ARCH}.deb
sudo apt install ./tisvlay_${VERSION}-1_${ARCH}.deb
```

### Arch / CachyOS

```sh
wget ${BASE}/tisvlay-${VERSION}-1-$(uname -m).pkg.tar.zst
sudo pacman -U ./tisvlay-${VERSION}-1-$(uname -m).pkg.tar.zst
```

### Fedora / RHEL

```sh
wget ${BASE}/tisvlay-${VERSION}-1.$(uname -m).rpm
sudo dnf install ./tisvlay-${VERSION}-1.$(uname -m).rpm
```

#### Bazzite / Silverblue

These immutable systems apply package changes after a reboot.

```sh
sudo rpm-ostree install ./tisvlay-${VERSION}-1.$(uname -m).rpm
```

### NixOS / Source Install

<details>
<summary id="nixos-flake-module">NixOS (flake module)</summary>

The repository's flake provides a NixOS module that wires up the daemon, user services, and CLI together.

```nix
# flake.nix
{
  inputs.tisvlay.url = "github:TisVlay/TisVlay";

  outputs = { nixpkgs, tisvlay, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      modules = [
        tisvlay.nixosModules.default
        {
          services.tisvlay.enable = true;
          services.tisvlay.users = [ "yourname" ];
        }
      ];
    };
  };
}
```

<details><summary id="prebuilt-binaries-optional">Prebuilt binaries (optional)</summary>

By default the module builds tisvlay against your system's nixpkgs, so it compiles locally on every install and update. Each [release](https://github.com/TisVlay/TisVlay/releases) is also built by CI and published to the public `tisvlay.cachix.org` binary cache, for x86_64 and aarch64 Linux. To use those builds instead, pin the input to a release tag, trust the cache, and point the module at the flake's own package:

```nix
# flake.nix
{
  inputs.tisvlay.url = "github:TisVlay/TisVlay/vX.Y.Z"; # a release tag
  # No `inputs.tisvlay.inputs.nixpkgs.follows`: it would change the build
  # and miss the cache.

  outputs = { nixpkgs, tisvlay, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      modules = [
        tisvlay.nixosModules.default
        ({ pkgs, ... }: {
          nix.settings.substituters = [ "https://tisvlay.cachix.org" ];
          nix.settings.trusted-public-keys = [
            "tisvlay.cachix.org-1:pa9AnLlywwXfm4cwUzRD1afQh2VbGTbYF6a9iQgzb/g="
          ];
          services.tisvlay.enable = true;
          services.tisvlay.package =
            tisvlay.packages.${pkgs.stdenv.hostPlatform.system}.default;
        })
      ];
    };
  };
}
```

The `nix.settings` lines take effect once a switch activates them, so pass the cache on the command line for the first switch, or it builds locally:

```sh
sudo nixos-rebuild switch --flake .#your-host \
  --option extra-substituters https://tisvlay.cachix.org \
  --option extra-trusted-public-keys tisvlay.cachix.org-1:pa9AnLlywwXfm4cwUzRD1afQh2VbGTbYF6a9iQgzb/g=
```

Later switches pick the cache up from `nix.settings` alone.

</details>

`services.tisvlay.enable` runs `tisvlay-daemon` (as the `tisvlay` user), the `tisvlay-session` and `tisvlay-tray` user services, and the `tisvlay` CLI in `$PATH`. The module's options, all under `services.tisvlay`:

| Option | What it does |
|--------|--------------|
| `users` | The durable [access](#access) grant: `tisvlay` group membership. List every account that should control TVs from SSH, scripts, or while not at the console. The console user has access anyway, but listing your user means access never depends on being the active seat. |
| `seatAccess` | The presence grant for the active seat's graphical user (on by default). Set `false` to restrict the socket to `users` and the greeter. |
| `settings` | Declarative TVs and profiles, rendered to a seed file (see [Configuration Files](../guide/configuration.md)). Re-applied on every start for TVs not already in the daemon's store, so runtime edits and pairing still persist. |
| `cec.enable`, `cec.pulse8.enable` | Device access for the [HDMI-CEC backend](#hdmi-cec-backend-optional); the second also builds the in-tree Pulse-Eight USB-CEC driver. |
| `greeter.enable` | The login-screen idle client (see [Automation](../guide/automation.md#login-screen-idle)). On automatically when plasma-login-manager or GDM is the display manager. |
| `openFirewall`, `bindPort` | Open the pinned discovery port in the host firewall (see [Firewall & Discovery](../operations/firewall-discovery.md)). |
| `sunshinePort` | Where to find Sunshine if you moved it off its default port (see [Streaming](../guide/streaming.md#sunshine-integration)). |
| `logLevel`, `extraArgs` | The services' log level, and extra `tisvlay-daemon` arguments for anything the module doesn't expose. |

Types and defaults are in each option's description in [`module.nix`](https://github.com/TisVlay/TisVlay/blob/main/packaging/nix/module.nix).

</details>

<details>
<summary id="cargo-from-source">cargo from source</summary>

```sh
git clone https://github.com/TisVlay/TisVlay tisvlay
cd tisvlay
cargo build --release
```

Binaries land in `target/release/`. Building from Cargo does **not** install the systemd units — copy those from [`packaging/`](https://github.com/TisVlay/TisVlay/tree/main/packaging) if you want the daemon and session services managed by systemd, and create the `tisvlay` group yourself.

A recent stable Rust toolchain is required — at least the `rust-version` declared in the workspace `Cargo.toml`; install it from <https://rust-lang.org/tools/install/>.

</details>

## After installation

After a fresh install, the system daemon starts automatically. The session and tray start at your next graphical login; to start them now:

```sh
systemctl --user start tisvlay-session tisvlay-tray
```

Packages also install shell completion for the `tisvlay` CLI (bash, zsh, fish); see the [Command Line](../guide/cli.md) page. Bash needs the `bash-completion` loader (recommended by the Debian and Fedora packages).

### Access

At the desktop, no extra setup is needed: the daemon grants access to the user at the active seat.

<details>
<summary>Set up access for SSH, scripts, or headless use</summary>

For SSH or headless use, grant your account persistent access, then run:

```sh
tisvlay enable-access
```

This adds your account to the `tisvlay` group through polkit. The group change takes effect at a fresh login; because some desktop services keep old group membership after logout, reboot if access is still denied.

</details>

## Optional Additions

<details>
<summary id="hdmi-cec-backend-optional">HDMI-CEC backend (optional)</summary>

The HDMI-CEC backend needs a CEC adapter that appears as a `/dev/cecN` character device. Most GPU HDMI ports don't wire the CEC pin. Two known-working sources:

- A **USB CEC adapter** (e.g. Pulse-Eight), using the kernel `pulse8-cec` driver. Pulse-Eight adapters also need `inputattach --pulse8-cec` to bind the USB serial device and create `/dev/cecN`; some distributions provide this setup already. See the [kernel's USB dongle instructions](https://docs.kernel.org/admin-guide/media/cec.html#usb-cec-dongles).
- An active **DisplayPort→HDMI adapter on an amdgpu or i915 output** — the GPU driver tunnels DP-AUX CEC. **Nvidia GPUs expose no CEC**, so use a USB adapter there.

The daemon's sandbox hides `/dev` by default, so it needs device access granted once:

- **NixOS:** set `services.tisvlay.cec.enable = true` for a GPU adapter. For a Pulse-Eight USB adapter, set `services.tisvlay.cec.pulse8.enable = true`; it also enables device access and attaches the driver.
- **Distro packages:** let the CEC devices through the sandbox with a drop-in (`sudo systemctl edit tisvlay-daemon`):

  ```ini
  [Service]
  PrivateDevices=false
  DeviceAllow=char-cec rw
  ```

  then give the `tisvlay` group read/write on the nodes with a udev rule, and restart the daemon:

  ```sh
  echo 'KERNEL=="cec[0-9]*", GROUP="tisvlay", MODE="0660"' | sudo tee /etc/udev/rules.d/70-tisvlay-cec.rules
  sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=cec
  sudo systemctl restart tisvlay-daemon
  ```

For a Pulse-Eight adapter, this grants the daemon access to `/dev/cecN` once it exists; the host still needs the `pulse8-cec` driver and `inputattach` setup described above.

For [the TV acting on your PC](../guide/tv-controls-pc.md), the packages and the NixOS module also install a polkit rule letting the daemon put the PC to sleep, a service that applies the TV-wakes-the-PC setting around each sleep, and a keymap keeping the TV remote's power keys off the desktop. A source build installs none of them: sleeping the PC and the wake setting don't work there, and turning on **Remote keys reach the desktop** lets the remote's power key reach the desktop, which usually sleeps the PC. The files and where they go are in [packaging/README.md](https://github.com/TisVlay/TisVlay/blob/main/packaging/README.md#the-tv-acting-on-the-pc).

The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Adding a TV](./pairing.md#hdmi-cec-no-pairing-needed).

</details>
