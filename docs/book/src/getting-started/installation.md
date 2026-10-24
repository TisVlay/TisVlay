# Installation

Most Linux users can install a pre-built package below. Choose the instructions for your distribution.

## Distro packages (.deb / .rpm / .pkg.tar.zst / flake)

Pre-built packages are published on the [releases page](https://github.com/TisVlay/TisVlay/releases) for each tagged version, for x86_64 and arm64 (aarch64). The commands below pick the one that matches your machine.

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

### Verify your download

From v0.4.0 on, every package is signed when it's built, through [Sigstore](https://www.sigstore.dev/). The signature proves which repository, commit and workflow produced the file. To check one with the [GitHub CLI](https://cli.github.com/), signed in to any GitHub account:

```sh
gh attestation verify tisvlay_${VERSION}-1_${ARCH}.deb \
  --repo TisVlay/TisVlay \
  --signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml \
  --source-ref refs/tags/v${VERSION}
```

- Put the file you downloaded in place of the `.deb`.
- Keep `--signer-workflow`. Without it, a signature from any workflow in the repository would pass, including ones that run pull-request code.
- The packages carry the binaries unchanged, so the same command also works on an installed system: `gh attestation verify /usr/bin/tisvlay …`.

Each binary also has a signed SBOM (software bill of materials): the crates it was built from, with their versions and licenses. To check it and print it, add the SBOM type:

```sh
gh attestation verify /usr/bin/tisvlay \
  --repo TisVlay/TisVlay \
  --signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml \
  --source-ref refs/tags/v${VERSION} \
  --predicate-type https://cyclonedx.org/bom \
  --format json --jq '.[0].verificationResult.statement.predicate'
```

The releases page has the same SBOMs as files: `tisvlay-${VERSION}-$(uname -m).cdx.json` and `tisvlay-gui-${VERSION}-$(uname -m).cdx.json`.

**Without a GitHub account**, use [cosign](https://docs.sigstore.dev/cosign/) with the signature bundle from the releases page:

```sh
wget ${BASE}/tisvlay-${VERSION}-$(uname -m).provenance.sigstore.json
cosign verify-blob-attestation \
  --bundle tisvlay-${VERSION}-$(uname -m).provenance.sigstore.json \
  --certificate-identity https://github.com/TisVlay/TisVlay/.github/workflows/build-packages.yml@refs/tags/v${VERSION} \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --type https://slsa.dev/provenance/v1 \
  tisvlay_${VERSION}-1_${ARCH}.deb
```

Releases are also immutable: once published, a release's files can't be replaced. `gh release verify-asset v${VERSION} <file>` checks that a file is one the release published.

On NixOS, Nix checks the cache's signing key on every prebuilt path it substitutes (see [Prebuilt binaries](#prebuilt-binaries-optional)), so there's nothing to verify by hand.

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
| `sunshinePort` | Where to find Sunshine if you moved it off its default port (see [Remote-Session Awareness](../guide/remote-sessions.md#sunshine-integration)). |
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

The backend depends on the LGPL-licensed [`linux-cec`](https://crates.io/crates/linux-cec) crate.

No pairing prompt or credentials are involved — a CEC TV is added by picking its adapter; see [Adding a TV](./pairing.md#hdmi-cec-no-pairing-needed).

</details>
