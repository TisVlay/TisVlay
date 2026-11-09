{
  description = "tisvlay - TV Display control for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Evaluates this flake for plain Nix: default.nix reads the pin from
    # flake.lock, so `nix-build` and `import` reach the same outputs.
    flake-compat = {
      url = "github:NixOS/flake-compat";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      # `f system` for each supported system, keyed the flake's way
      # (`packages.${system}`). Linux only, as the project is;
      # tisvlay.cachix.org serves both.
      eachSystem =
        f:
        let
          bySystem = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] f;
        in
        nixpkgs.lib.genAttrs [ "packages" "checks" "devShells" ] (
          output: nixpkgs.lib.mapAttrs (_: outputs: outputs.${output}) bySystem
        );
    in
    # System-agnostic outputs
    {
      nixosModules.default = import ./packaging/nix/module.nix;

      overlays.default = final: prev: {
        tisvlay = final.callPackage ./packaging/nix/package.nix { };
      };
    }
    # Per-system outputs (packages, checks, devShell)
    // eachSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit overlays system; };
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        # LLVM bintools for cargo-llvm-cov (llvm-cov / llvm-profdata), pinned
        # once here to the major matching this toolchain's rustc; keep in step
        # on a channel bump.
        llvmPkgs = pkgs.llvmPackages_23;
        # Slint 1.19 ahead of its crates.io release: the dev shell's viewer
        # and LSP build from the revision the workspace's `slint` is patched
        # to (root Cargo.toml, `[patch.crates-io]`), so the tools compile the
        # UI with the app's compiler. Both go back to the registry releases
        # when 1.19.0 is published.
        slintSrc = pkgs.fetchFromGitHub {
          owner = "slint-ui";
          repo = "slint";
          rev = "b4448d86acb4329486e1515d8e35544cd081b9c4";
          hash = "sha256-UYh/E77v7Gx3/4WhOG5QuP4DZ5mduiV24odgVPD3TSc=";
        };
        slintCargoDeps = pkgs.rustPlatform.fetchCargoVendor {
          src = slintSrc;
          hash = "sha256-5SAXsaKRxITsGHjE3iZkF10cdAonr6h36BOJDVInW38=";
        };
      in
      {
        packages = {
          # The `dist` profile: smaller binaries for the build CI pushes to
          # tisvlay.cachix.org. The overlay and the module's fallback keep
          # `release`, since those hosts compile it themselves.
          tisvlay = pkgs.callPackage ./packaging/nix/package.nix { cargoProfile = "dist"; };
          default = self.packages.${system}.tisvlay;

          # Out-of-tree build of the in-tree pulse8-cec HDMI-CEC driver, so
          # Pulse-Eight USB-CEC adapters expose /dev/cecN without a full
          # kernel rebuild (nixpkgs doesn't ship USB_PULSE8_CEC). Built here
          # against nixpkgs' default kernel for `nix build .#pulse8-cec`
          # validation; the NixOS module rebuilds it against the host kernel
          # (config.boot.kernelPackages) so vermagic matches at runtime. See
          # services.tisvlay.cec.pulse8.enable.
          pulse8-cec = pkgs.linuxPackages.callPackage ./packaging/nix/pulse8-cec.nix { };

          # slint-viewer and slint-lsp at the workspace's slint version;
          # nixpkgs' lag behind it. Exposed so the devShell and CI can share
          # one definition.
          slint-viewer = pkgs.callPackage ./packaging/nix/slint-viewer.nix {
            inherit slintSrc slintCargoDeps;
          };
          slint-lsp = pkgs.callPackage ./packaging/nix/slint-lsp.nix {
            inherit slintSrc slintCargoDeps;
          };

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
        # on the flake's own package, the `dist` build tisvlay.cachix.org
        # serves, and asserts the module's end-to-end behavior. Run via
        # `nix build .#checks.${system}.vm` or `nix flake check`.
        checks.vm = pkgs.callPackage ./packaging/nix/test.nix {
          package = self.packages.${system}.tisvlay;
        };

        # Cargo unit + integration suite in the Nix sandbox. The package
        # ships with doCheck = false (most consumers build from source and
        # shouldn't re-run CI's suite); this output keeps it
        # reachable via `nix flake check` and for a future nixpkgs submission.
        checks.tests = self.packages.${system}.tisvlay.overrideAttrs (_: {
          doCheck = true;
        });

        devShells.default =
          let
            slintRuntimeLibs = pkgs.callPackage ./packaging/nix/slint-runtime.nix { };
            slintViewer = self.packages.${system}.slint-viewer;
            slintLsp = self.packages.${system}.slint-lsp;
            # The same pinned cargo-about the package builds with, so the
            # dev shell's license bundle matches the shipped one.
            cargoAbout = pkgs.callPackage ./packaging/nix/cargo-about.nix { };
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
                # docs/licensing.md. The pinned one, via cargoAbout.
                cargoAbout
                cargo-deny
                # cargo-hack checks each feature in isolation (`just hack`);
                # every other gate builds with --all-features.
                cargo-hack
                # nixpkgs' cargo-llvm-cov fails its own test suite (the
                # instrumented test binaries segfault under nixpkgs' rustc),
                # so the binary cache has no build of it; skip those tests.
                # Drop the override once nixpkgs builds it again.
                (pkgs.cargo-llvm-cov.overrideAttrs { doCheck = false; })
                cargo-nextest
                # cargo-shear finds unused and misplaced dependencies
                # (`just shear`) by parsing sources; no build needed.
                cargo-shear
                # The release build's supply-chain tools: cargo-auditable embeds
                # each binary's dependency list (`just package-*`), and
                # cargo-cyclonedx writes the per-binary SBOMs that `just
                # sbom-check` compares with it through rust-audit-info (and jq).
                # release.yml installs the same versions.
                cargo-auditable
                cargo-cyclonedx
                rust-audit-info
                jq
                # uuidgen gives each SBOM its serial number (`just sbom`).
                util-linux
                # Release verification (`just verify-assets`, `just
                # verify-release`): gh checks the attestations, bsdtar (from
                # libarchive) unpacks all three package formats, and cosign
                # runs the book's check for people without a GitHub account.
                gh
                libarchive
                cosign
                # Release prep (`release-plz update`): the version bump and
                # the CHANGELOGs from the conventional-commit history;
                # cargo-semver-checks lets it check the published crates for
                # API breaks.
                release-plz
                cargo-semver-checks
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
                # Read the Docs). See justfile's `docs-*` recipes.
                # mdbook-mermaid renders ```mermaid blocks; its JS assets are
                # gitignored and regenerated by `mdbook-mermaid install` at
                # build time.
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
                # instead of failing to find a bundled binary, at the
                # workspace's slint version (packaging/nix/slint-lsp.nix).
                # Tracks slint-ui/slint#2577 (NixOS LSP discovery).
                slintLsp
                # slint-viewer at the workspace's slint version
                # (packaging/nix/slint-viewer.nix) for headless `--check` /
                # `--screenshot` during GUI work.
                slintViewer
                socat
                tcpdump
                # TOML formatter (`just fmt`; config in .config/tombi.toml),
                # pinned above.
                tombi
                # Spell checker for code, docs and identifiers (`just typos`;
                # config in .config/typos.toml).
                typos
                # Workflow linters (`just actionlint`, `just zizmor`).
                # actionlint runs shellcheck over the `run:` scripts when it's
                # on PATH. ci.yml's `checks` job installs the same versions.
                actionlint
                shellcheck
                zizmor
                # Pins a newly added action to its commit SHA, with the
                # release in a comment (docs/build.md § Workflows).
                pinact
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
