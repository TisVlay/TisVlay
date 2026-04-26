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
        packages = {
          tisvlay = pkgs.callPackage ./packaging/nix/package.nix { };
          default = self.packages.${system}.tisvlay;
        };

        # NixOS VM test: boots a VM with services.tisvlay.enable = true
        # and asserts the module's end-to-end behavior. Run via
        # `nix build .#checks.${system}.vm` or `nix flake check`.
        checks.vm = pkgs.callPackage ./packaging/nix/test.nix { };

        devShells.default =
          let
            # System libs Slint dlopens at build + run time on Linux.
            # Tracks the runtime-libs set in upstream's own dev shell at
            # libs/slint/flake.nix so we don't drift on what's needed.
            #   - fontconfig: femtovg renderer (default)
            #   - libGL:      OpenGL ES backend
            #   - libxkbcommon + wayland: winit Wayland backend
            #   - libxcb / libX11 / libXcursor / libXi / libXrandr:
            #     winit X11 backend (X11 sessions only)
            slintRuntimeLibs = with pkgs; [
              fontconfig
              libGL
              libxkbcommon
              wayland
              libxcb
              libx11
              libxcursor
              libxi
              libxrandr
            ];
          in
          pkgs.mkShell {
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
              prek
              # slint-lsp on $PATH so editor integrations (VSCode Slint
              # extension, Helix, Neovim) pick up the system-provided LSP
              # instead of failing to find a bundled binary.
              # Tracks slint-ui/slint#2577 (NixOS LSP discovery).
              slint-lsp
              socat
              tcpdump
              wakeonlan
              rust
            ] ++ slintRuntimeLibs;

            # Tell cargo where to find the musl linker
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsMusl.stdenv.cc}/bin/cc";

            # Tell rust-analyzer where to find std sources for go-to-definition.
            RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";

            # Tell cargo-llvm-cov where to find LLVM tools matching rustc's LLVM 21.
            # rustup's llvm-tools-preview is not available with the Nix-managed
            # toolchain, so we point at nixpkgs' LLVM 21 bintools instead.
            LLVM_COV = "${pkgs.llvmPackages_21.bintools-unwrapped}/bin/llvm-cov";
            LLVM_PROFDATA = "${pkgs.llvmPackages_21.bintools-unwrapped}/bin/llvm-profdata";

            # Slint's femtovg renderer dlopens libGL / wayland / xcb at runtime;
            # without LD_LIBRARY_PATH the GUI binary built in the dev shell
            # can't find them on a non-NixOS host or in a pure shell.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath slintRuntimeLibs;
          };
      }
    );
}
