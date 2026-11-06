{
  description = "tisvlay - TV Display control for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # default.nix defines everything, and reads its pins from this flake's
  # lock; the flake maps its attributes onto the flake output schema.
  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      # Linux only, as the project is; tisvlay.cachix.org serves both.
      forEachSystem =
        f:
        nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
          system: f (import ./. { inherit system nixpkgs rust-overlay; })
        );
    in
    {
      nixosModules.default = import ./packaging/nix/module.nix;
      overlays.default = import ./packaging/nix/overlay.nix;

      packages = forEachSystem (t: {
        inherit (t)
          tisvlay
          pulse8-cec
          slint-viewer
          msrv-rust
          ;
        default = t.tisvlay;
      });

      # `nix flake check`: the VM test and the sandboxed test suite.
      checks = forEachSystem (t: t.checks);

      devShells = forEachSystem (t: {
        default = t.shell;
      });
    };
}
