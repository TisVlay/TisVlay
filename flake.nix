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
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        # LLVM bintools for cargo-llvm-cov (llvm-cov / llvm-profdata), pinned
        # once here to the major matching this toolchain's rustc; keep in step
        # on a channel bump.
        llvmPkgs = pkgs.llvmPackages_23;
      in
      {
        packages = {
          tisvlay = pkgs.callPackage ./packaging/nix/package.nix { };
          default = self.packages.${system}.tisvlay;

          # Out-of-tree build of the in-tree pulse8-cec HDMI-CEC driver, so
          # Pulse-Eight USB-CEC adapters expose /dev/cecN without a full
          # kernel rebuild (nixpkgs doesn't ship USB_PULSE8_CEC). Built here
          # against nixpkgs' default kernel for `nix build .#pulse8-cec`
          # validation; the NixOS module rebuilds it against the host kernel
          # (config.boot.kernelPackages) so vermagic matches at runtime. See
          # services.tisvlay.cec.pulse8.enable.
          pulse8-cec = pkgs.linuxPackages.callPackage ./packaging/nix/pulse8-cec.nix { };

          # slint-viewer pinned to the workspace's slint version (1.18.1);
          # nixpkgs' is 1.16.1, which predates `--check` / `--screenshot`.
          # Exposed so the devShell and CI can share one definition.
          slint-viewer = pkgs.callPackage ./packaging/nix/slint-viewer.nix { };

          # The workspace's MSRV toolchain, for `just msrv`. The version is
          # read from Cargo.toml's `rust-version` (padded to x.y.z), so the
          # floor lives in one place.
          msrv-rust =
            let
              floor = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.rust-version;
              parts = builtins.length (builtins.splitVersion floor);
            in
            pkgs.rust-bin.stable.${if parts == 2 then "${floor}.0" else floor}.minimal;
        };

        # NixOS VM test: boots a VM with services.tisvlay.enable = true
        # and asserts the module's end-to-end behavior. Run via
        # `nix build .#checks.${system}.vm` or `nix flake check`.
        checks.vm = pkgs.callPackage ./packaging/nix/test.nix { };

        # Cargo unit + integration suite in the Nix sandbox. The package
        # ships with doCheck = false (no binary cache means consumers build
        # from source and shouldn't re-run CI's suite); this output keeps it
        # reachable via `nix flake check` and for a future nixpkgs submission.
        checks.tests = self.packages.${system}.tisvlay.overrideAttrs (_: {
          doCheck = true;
        });

        devShells.default =
          let
            slintRuntimeLibs = pkgs.callPackage ./packaging/nix/slint-runtime.nix { };
            slintViewer = self.packages.${system}.slint-viewer;
            # cargo-about pinned to the first release whose generator honors
            # LicenseRef-* clarify files (0.9.1) — without it the vendored Slint
            # crates' royalty-free license text is dropped from the generated
            # bundle. nixpkgs still lags on 0.9.0; drop this override once it
            # ships >= 0.9.1.
            cargoAboutSrc = pkgs.fetchFromGitHub {
              owner = "EmbarkStudios";
              repo = "cargo-about";
              tag = "0.9.1";
              hash = "sha256-mZyfECnlpYom/XI445DOeUAthqK3HKC+QkkK+5gf0/E=";
            };
            cargoAbout = pkgs.cargo-about.overrideAttrs {
              version = "0.9.1";
              src = cargoAboutSrc;
              cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
                src = cargoAboutSrc;
                hash = "sha256-//t3ASNljC4rosk5oOmcJy8PZKRYjlQ0euzzAmfiSPQ=";
              };
            };
            cargoNfpmSrc = pkgs.fetchCrate {
              pname = "cargo-nfpm";
              version = "0.1.3";
              hash = "sha256-PpflbxUL6/xwqeHw2OhPLcB1fi0VLbItzhdYkLqZuP0=";
            };
            cargoNfpm = pkgs.callPackage (import "${cargoNfpmSrc}/nix/package.nix") { };
            # tombi (TOML formatter, `just fmt`) pinned to the version CI
            # installs (`tombi@1.7.1` in ci.yml), so local and CI formatting
            # can't disagree; nixpkgs lags behind. Bump both together, and
            # drop this override once nixpkgs reaches the pin.
            tombiSrc = pkgs.fetchFromGitHub {
              owner = "tombi-toml";
              repo = "tombi";
              tag = "v1.7.1";
              hash = "sha256-eeBtmMrkDWOADw7FMLLD8hYq5WYg/aFyATtYIMFYFk0=";
            };
            tombi = pkgs.tombi.overrideAttrs {
              version = "1.7.1";
              src = tombiSrc;
              cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
                src = tombiSrc;
                hash = "sha256-YJLhWIwB3Y8836Rq4ahRzBSnhp7bH8w2/oXfRVTOPXg=";
              };
            };
          in
          pkgs.mkShell {
            packages =
              with pkgs;
              [
                # cargo-about generates the bundled third-party license
                # attribution; cargo-deny enforces the SPDX allow-list
                # at PR time via prek so a future dep bump can't drag
                # in a forbidden license. Maintenance reference:
                # docs/licensing.md. Pinned to 0.9.1 via cargoAbout (above).
                cargoAbout
                cargo-deny
                # cargo-hack checks each feature in isolation (`just hack`);
                # every other gate builds with --all-features.
                cargo-hack
                cargo-llvm-cov
                cargo-nextest
                # cargo-shear finds unused and misplaced dependencies
                # (`just shear`) by parsing sources; no build needed.
                cargo-shear
                # cargo-sbom emits a whole-workspace SBOM (SPDX) for `just sbom`.
                cargo-sbom
                # Cargo plugin that reads package metadata and invokes nfpm
                # for `just package-*`; source and lockfile are pinned above.
                cargoNfpm
                # cargo-zigbuild + zig drive the release build: zig serves
                # as the linker so the resulting glibc-dynamic binary
                # carries an explicit min-glibc baseline (see justfile's
                # `package-build`) and runs on any distro at-or-newer than
                # that anchor — without baking Nix-store paths into the
                # ELF interpreter the way a plain `cargo build` from the
                # dev shell would.
                cargo-zigbuild
                zig
                just
                just-lsp
                linuxConsoleTools
                # mdBook builds the user-facing docs site (docs/book →
                # GitHub Pages). See justfile's `docs-*` recipes and
                # docs/plans/mdbook-docs-site.md. mdbook-mermaid renders
                # ```mermaid blocks; its JS assets are gitignored and
                # regenerated by `mdbook-mermaid install` at build time.
                mdbook
                mdbook-mermaid
                # Checks local links and anchors in the generated book HTML.
                lychee
                lldb
                llvmPkgs.bintools-unwrapped
                # nfpm (Go binary) is the packager that cargo-nfpm shells out to.
                nfpm
                # pipewire's `libspa-0.2` pkg-config entry gives dev builds
                # a real SPA plugin dir for the session's PipeWire client.
                pipewire
                pkg-config
                prek
                # slint-lsp on $PATH so editor integrations (VSCode Slint
                # extension, Helix, Neovim) pick up the system-provided LSP
                # instead of failing to find a bundled binary.
                # Tracks slint-ui/slint#2577 (NixOS LSP discovery).
                slint-lsp
                # slint-viewer pinned to 1.18.1 (packaging/nix/slint-viewer.nix)
                # for headless `--check` / `--screenshot` during GUI work.
                slintViewer
                socat
                tcpdump
                # TOML formatter (`just fmt`; config in .config/tombi.toml),
                # pinned above.
                tombi
                # Spell checker for code, docs and identifiers (`just typos`;
                # config in .config/typos.toml).
                typos
                wakeonlan
                rust
                # i18n tooling. `gettext` provides xgettext / msgfmt /
                # msgmerge for building .pot / .po / .mo from Rust + Slint
                # source. `slint-tr-extractor` walks .slint files and
                # emits the equivalent .pot for `@tr()` literals; the
                # justfile recipe combines both into a single catalog.
                gettext
                slint-tr-extractor
              ]
              ++ slintRuntimeLibs;

            # Tell rust-analyzer where to find std sources for go-to-definition.
            RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";

            # cargo-llvm-cov needs llvm-cov / llvm-profdata whose LLVM major
            # matches rustc's (see llvmPkgs above); rustup's llvm-tools-preview
            # isn't available under the Nix toolchain, so point at nixpkgs'.
            LLVM_COV = "${llvmPkgs.bintools-unwrapped}/bin/llvm-cov";
            LLVM_PROFDATA = "${llvmPkgs.bintools-unwrapped}/bin/llvm-profdata";

            # Slint's femtovg renderer dlopens libGL / wayland / xcb at runtime;
            # without LD_LIBRARY_PATH the GUI binary built in the dev shell
            # can't find them on a non-NixOS host or in a pure shell.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath slintRuntimeLibs;
          };
      }
    );
}
