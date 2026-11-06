# The Nix entry point. Works without flakes; flake.nix only maps these
# attributes per system.
#
#   nix-build -A tisvlay      the package (the build tisvlay.cachix.org serves)
#   nix-shell                 the contributor dev shell (shell.nix)
#   nix-build -A checks.vm    the NixOS VM test
#
# For a NixOS host, import `module` (packaging/nix/module.nix) from a
# fetched copy of this repository; packaging/nix/README.md has the recipes.
let
  lock = builtins.fromJSON (builtins.readFile ./flake.lock);

  # A flake.lock input, fetched and checked against its narHash, so
  # `nix flake update` moves this entry point's pins along with the flake's.
  locked =
    name:
    let
      node = lock.nodes.${lock.nodes.root.inputs.${name}}.locked;
    in
    assert node.type == "github";
    builtins.fetchTarball {
      url = "https://github.com/${node.owner}/${node.repo}/archive/${node.rev}.tar.gz";
      sha256 = node.narHash;
    };
in
{
  system ? builtins.currentSystem,
  nixpkgs ? locked "nixpkgs",
  rust-overlay ? locked "rust-overlay",
  # Passing your own nixpkgs builds the package against it, as the module's
  # default does; it then misses the binary cache.
  pkgs ? import nixpkgs { inherit system; },
}:

let
  # rust-overlay's toolchains, for the dev shell and the MSRV check only:
  # the package builds with nixpkgs' own rustc.
  rustBin = (if pkgs ? rust-bin then pkgs else pkgs.extend (import rust-overlay)).rust-bin;

  self = {
    # The `dist` profile: smaller binaries for the build CI pushes to
    # tisvlay.cachix.org. The overlay and the module's fallback keep
    # `release`, since those hosts compile it themselves.
    tisvlay = pkgs.callPackage ./packaging/nix/package.nix { cargoProfile = "dist"; };

    # Out-of-tree build of the in-tree pulse8-cec HDMI-CEC driver, so
    # Pulse-Eight USB-CEC adapters expose /dev/cecN without a full
    # kernel rebuild (nixpkgs doesn't ship USB_PULSE8_CEC). Built here
    # against nixpkgs' default kernel for validation; the NixOS module
    # rebuilds it against the host kernel (config.boot.kernelPackages) so
    # vermagic matches at runtime. See services.tisvlay.cec.pulse8.enable.
    pulse8-cec = pkgs.linuxPackages.callPackage ./packaging/nix/pulse8-cec.nix { };

    # slint-viewer pinned to the workspace's slint version (1.18.1);
    # nixpkgs' is 1.16.1, which predates `--check` / `--screenshot`.
    # Exposed so the dev shell and CI can share one definition.
    slint-viewer = pkgs.callPackage ./packaging/nix/slint-viewer.nix { };

    # The workspace's MSRV toolchain, for `just msrv`. The version is
    # read from Cargo.toml's `rust-version` (padded to x.y.z), so the
    # floor lives in one place.
    msrv-rust =
      let
        floor = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.rust-version;
        parts = builtins.length (builtins.splitVersion floor);
      in
      rustBin.stable.${if parts == 2 then "${floor}.0" else floor}.minimal;

    checks = {
      # NixOS VM test: boots a VM with services.tisvlay.enable = true on
      # `tisvlay` above, the build tisvlay.cachix.org serves, and asserts
      # the module's end-to-end behavior.
      vm = pkgs.callPackage ./packaging/nix/test.nix { package = self.tisvlay; };

      # Cargo unit + integration suite in the Nix sandbox. The package
      # ships with doCheck = false (most consumers build from source and
      # shouldn't re-run CI's suite); this keeps it reachable, and ready
      # for a future nixpkgs submission.
      tests = self.tisvlay.overrideAttrs (_: {
        doCheck = true;
      });
    };

    shell = import ./packaging/nix/devshell.nix {
      inherit pkgs;
      inherit (self) slint-viewer;
      rust = rustBin.fromRustupToolchainFile ./rust-toolchain.toml;
    };

    module = import ./packaging/nix/module.nix;
    overlay = import ./packaging/nix/overlay.nix;
  };
in
self
