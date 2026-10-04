# Installation

tisvlay ships as pre-built distro packages, a NixOS module, and as source via Cargo. Pick the path that matches your system.

A distro-package install is zero-setup: the post-install hook enables and starts the system daemon (on Arch, start it once with `sudo systemctl start tisvlay-daemon` — pacman convention is that packages never start services), and the per-user services start automatically at your next graphical login. To start them right away without re-logging in:

```sh
systemctl --user start tisvlay-session tisvlay-tray
```

<!-- TisVlay components use [Zenoh](https://zenoh.io/) over Unix sockets for IPC, where the daemon owns the system socket, and the session service owns a per-user socket. Socket permission is the access model, with default access scoped to the user of the active desktop session or users in the `tisvlay` group. -->

The daemon runs as a dedicated `tisvlay` system user and stores its state under `/var/lib/tisvlay/`. Access to its control socket is layered:

- **At the console you already have access** — the daemon grants the active seat's logged-in user access automatically, the same way Linux grants the console user its sound card and camera. Nothing to configure.
- **For SSH, remote, scripts, or headless use**, join the **`tisvlay` group** — the durable grant. One click:

```sh
tisvlay enable-access   # authenticates via polkit, adds you to the tisvlay group
```

A group change only takes effect on a *fresh* login — and on most desktops the systemd user manager survives a plain logout with the old groups, so a **reboot** is the reliable way to pick it up. (Console access needs none of this.)

## Distro packages (.deb / .rpm / .pkg.tar.zst)

Pre-built packages are published on the [releases page](https://github.com/ruffsl/tisvlay/releases) for each tagged version. They are built against a conservative glibc baseline (2.34), so one package runs on every in-support desktop and enterprise distro, immutable-root variants included.

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

The post-install hook creates the `tisvlay` system user and group, enables and starts `tisvlay-daemon` (fresh installs only — an upgrade never overrides a daemon you deliberately stopped; on Arch you start it once yourself), and enables the per-user services for everyone's next graphical login. It deliberately adds no one to the `tisvlay` group — the console user doesn't need it, and `tisvlay enable-access` (above) covers everyone else.

The packages also install shell **tab completion** for the `tisvlay` CLI (bash, zsh, fish) — `<TAB>` completes verbs, flags, and even live values like TV and profile ids, queried from the running daemon and described by name. Bash needs the `bash-completion` loader package (the `.deb`/`.rpm` recommend it); zsh and fish work out of the box.

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

`services.tisvlay.enable` runs `tisvlay-daemon` (as the `tisvlay` user), the `tisvlay-session` and `tisvlay-tray` user services, and the `tisvlay` CLI in `$PATH`. **`services.tisvlay.users` is the durable grant** (`tisvlay` group membership) — list every account that should control TVs from SSH, scripts, or while not at the console. The user at the console gets access automatically either way (the presence grant below), so a single-user desktop works with just `enable = true`; listing your user is still recommended so access never depends on being the active seat.

Other useful options:

- `services.tisvlay.seatAccess` — the presence grant: the active seat's graphical user can reach the daemon while at the console, with no group membership (default `true`; set `false` to restrict the socket to the `users` list and the greeter).
- `services.tisvlay.cec.enable` — device access for the HDMI-CEC backend (see below); `services.tisvlay.cec.pulse8.enable` additionally builds the in-tree Pulse-Eight USB-CEC driver.
- `services.tisvlay.greeter.enable` — the optional login-screen idle client (defaults on when `plasma-login-manager` **or GDM/GNOME** is the display manager).
- `services.tisvlay.openFirewall` / `services.tisvlay.bindPort` — open the pinned SSDP discovery source port (default `1901`); see [Firewall & Discovery](../operations/firewall-discovery.md).

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

On NixOS, enable device access for the sandboxed daemon with `services.tisvlay.cec.enable = true` (it installs the udev rule granting the `tisvlay` group read/write on `/dev/cec*` and permits the char-cec subsystem through the daemon's hardened unit). For a Pulse-Eight USB adapter, `services.tisvlay.cec.pulse8.enable = true` also builds the in-tree driver. The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Pairing](./pairing.md#hdmi-cec-no-pairing-needed).
