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
      nixosModules.default = import ./packaging/nix/module.nix;

      overlays.default = final: prev: {
        tisvlay = final.callPackage ./packaging/nix/package.nix { };
      };
    }
    # Per-system outputs (packages, devShell)
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit overlays system; };
        rust = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
          targets = [ "x86_64-unknown-linux-musl" ];
        };
      in
      {
        packages.default = pkgs.callPackage ./packaging/nix/package.nix { };

        # Out-of-tree kernel module providing /dev/tisvlay-bl and per-TV
        # /sys/class/backlight/tisvlay-* devices. Built against the
        # default `linuxPackages` of the flake's nixpkgs — consumers
        # who need it aligned with a different kernel should override
        # `services.tisvlay.kernelModulePackage` in the NixOS module.
        packages.tisvlay-backlight =
          pkgs.linuxPackages.callPackage ./module/tisvlay-backlight { };

        # NixOS VM test: boots a VM with services.tisvlay.enable = true
        # and asserts the module's end-to-end behavior. Run via
        # `nix build .#checks.${system}.vm` or `nix flake check`.
        checks.vm = pkgs.callPackage ./packaging/nix/test.nix { };

        devShells.powerdevil = pkgs.mkShell {
          inputsFrom = [ pkgs.kdePackages.powerdevil ];
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo-insta
            cargo-llvm-cov
            cargo-nextest
            just
            just-lsp
            linuxConsoleTools
            lldb
            llvmPackages_21.bintools-unwrapped
            pkg-config
            pkgsMusl.stdenv.cc
            socat
            tcpdump
            wakeonlan
            rust
          ];

          # Tell cargo where to find the musl linker
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";

          # Tell cargo-llvm-cov where to find LLVM tools matching rustc's LLVM 21.
          # rustup's llvm-tools-preview is not available with the Nix-managed
          # toolchain, so we point at nixpkgs' LLVM 21 bintools instead.
          LLVM_COV = "${pkgs.llvmPackages_21.bintools-unwrapped}/bin/llvm-cov";
          LLVM_PROFDATA = "${pkgs.llvmPackages_21.bintools-unwrapped}/bin/llvm-profdata";
        };
      }
    );
}
