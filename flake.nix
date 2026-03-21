{
  description = "tisvlay - TV Display control for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    # System-agnostic outputs
    {
      nixosModules.default = import ./nix/module.nix;

      overlays.default = final: prev: {
        tisvlay = final.callPackage ./nix/package.nix { };
      };
    }
    # Per-system outputs (packages, devShell)
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit overlays system; };
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        packages.default = pkgs.callPackage ./nix/package.nix { };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            appstream
            flatpak-builder
            librsvg
            linuxConsoleTools
            lldb
            pkg-config
            socat
            tcpdump
            wakeonlan
            python313Packages.aiohttp
            python313Packages.tomlkit
            rust
          ];
          # Use librsvg's gdk-pixbuf loader cache file as it enables gdk-pixbuf to load
          # SVG files (important for icons)
          # Fixes error: .../share/icons/hicolor/scalable/apps/blabla.svg is not a valid icon: Format not recognized
          GDK_PIXBUF_MODULE_FILE = "${pkgs.librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache";
        };
      }
    );
}
