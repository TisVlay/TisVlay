# Installation

tisvlay ships as pre-built distro packages, a NixOS module, and as source via Cargo. Pick the path that matches your system.

A distro-package install is zero-setup: the post-install hook enables and starts the system daemon (on Arch, start it once with `sudo systemctl start tisvlay-daemon` — pacman convention is that packages never start services), and the per-user services start automatically at your next graphical login. To start them right away without re-logging in:

```sh
systemctl --user start tisvlay-session tisvlay-tray
```

## Access

The daemon runs as a dedicated `tisvlay` system user, keeps its state under `/var/lib/tisvlay/`, and is controlled over a local socket. Access to that socket is layered:

- **At the console you already have access** — the daemon grants the active seat's logged-in user access automatically, the same way Linux grants the console user its sound card and camera. Nothing to configure. With fast user switching, the grant follows whoever is in the foreground.
- **For SSH, remote, scripts, or headless use**, join the **`tisvlay` group** — the durable grant. One click:

```sh
tisvlay enable-access   # authenticates via polkit, adds you to the tisvlay group
```

A group change only takes effect on a *fresh* login — and on most desktops the systemd user manager survives a plain logout with the old groups, so a **reboot** is the reliable way to pick it up. (Console access needs none of this.) The login screen's greeter client has its own scoped grant.

On NixOS, list accounts in `services.tisvlay.users` instead (see [below](#nixos-flake-module)).

## Distro packages (.deb / .rpm / .pkg.tar.zst)

Pre-built packages are published on the [releases page](https://github.com/ruffsl/tisvlay/releases) for each tagged version. They are built against a conservative glibc baseline, so one package runs on every in-support desktop and enterprise distro, immutable-root variants included.

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

The post-install hook creates the `tisvlay` system user and group, enables and starts `tisvlay-daemon` (fresh installs only — an upgrade never overrides a daemon you deliberately stopped), and enables the per-user services for everyone's next graphical login. It deliberately adds no one to the `tisvlay` group (see [Access](#access)).

The packages also install shell **tab completion** for the `tisvlay` CLI (bash, zsh, fish); see the [CLI Reference](../guide/cli.md). Bash needs the `bash-completion` loader package (the `.deb`/`.rpm` recommend it); zsh and fish work out of the box.

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
          services.tisvlay.users = [ "yourname" ];
        }
      ];
    };
  };
}
```

`services.tisvlay.enable` runs `tisvlay-daemon` (as the `tisvlay` user), the `tisvlay-session` and `tisvlay-tray` user services, and the `tisvlay` CLI in `$PATH`. The module's options, all under `services.tisvlay`:

| Option | What it does |
|--------|--------------|
| `users` | The durable [access](#access) grant: `tisvlay` group membership. List every account that should control TVs from SSH, scripts, or while not at the console. The console user has access anyway, but listing your user means access never depends on being the active seat. |
| `seatAccess` | The presence grant for the active seat's graphical user (on by default). Set `false` to restrict the socket to `users` and the greeter. |
| `settings` | Declarative TVs and profiles, rendered to a seed file (see [Configuration](../guide/configuration.md)). Re-applied on every start for TVs not already in the daemon's store, so runtime edits and pairing still persist. |
| `cec.enable`, `cec.pulse8.enable` | Device access for the [HDMI-CEC backend](#hdmi-cec-backend-optional); the second also builds the in-tree Pulse-Eight USB-CEC driver. |
| `greeter.enable` | The login-screen idle client (see [Session Automation](../guide/session-automation.md#login-screen-idle-greeter)). On automatically when plasma-login-manager or GDM is the display manager. |
| `openFirewall`, `bindPort` | Open the pinned discovery port in the host firewall (see [Firewall & Discovery](../operations/firewall-discovery.md)). |
| `sunshinePort` | Where to find Sunshine if you moved it off its default port (see [Remote-Session Awareness](../guide/remote-sessions.md#sunshine-integration)). |
| `logLevel`, `extraArgs` | The services' log level, and extra `tisvlay-daemon` arguments for anything the module doesn't expose. |

Types and defaults are in each option's description in [`module.nix`](https://github.com/ruffsl/tisvlay/blob/main/packaging/nix/module.nix).

## Cargo (from source)

```sh
git clone https://github.com/ruffsl/tisvlay
cd tisvlay
cargo build --release
```

Binaries land in `target/release/`. Building from Cargo does **not** install the systemd units — copy those from [`packaging/`](https://github.com/ruffsl/tisvlay/tree/main/packaging) if you want the daemon and session services managed by systemd, and create the `tisvlay` group yourself.

A recent stable Rust toolchain is required — at least the `rust-version` declared in the workspace `Cargo.toml`; install it from <https://rust-lang.org/tools/install/>.

## HDMI-CEC backend (optional)

The HDMI-CEC backend needs a CEC adapter that the kernel exposes as a `/dev/cecN` character device — most GPU HDMI ports don't wire the CEC pin. Two known-working sources:

- A **USB CEC adapter** (e.g. Pulse-Eight), via the kernel `pulse8-cec` driver.
- An active **DisplayPort→HDMI adapter on an amdgpu or i915 output** — the GPU driver tunnels DP-AUX CEC. **Nvidia GPUs expose no CEC**, so use a USB adapter there.

The daemon's sandbox hides `/dev` by default, so it needs device access granted once:

- **NixOS:** `services.tisvlay.cec.enable = true` does it all.
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

The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Pairing](./pairing.md#hdmi-cec-no-pairing-needed).
