# `slint-viewer` pinned to the same version as the workspace's `slint`
# dependency (crates/tisvlay-gui/Cargo.toml → =1.17.0). nixpkgs ships
# slint-viewer 1.16.1 (behind slint-lsp's 1.17.0), and 1.17 is the first
# release with `--check` / `--screenshot`, which the GUI QA workflow uses
# for headless render + compile checks. Mirrors the nixpkgs recipe
# (pkgs/by-name/sl/slint-viewer) with the version bumped.
{
  lib,
  rustPlatform,
  fetchCrate,

  fontconfig,
  libGL,
  pkg-config,
  qt6,
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "slint-viewer";
  version = "1.17.0";

  src = fetchCrate {
    inherit (finalAttrs) pname version;
    hash = "sha256-2Zjt7kt9tCxDyixa7Az0cPXh3noR2BN7Nxrq2bdxkSs=";
  };

  cargoHash = "sha256-KaVPQ14QQpabaJVsyhsQ3j546j63Lp16AzQtuz49niY=";

  buildInputs = [
    qt6.qtbase
    qt6.qtsvg
    fontconfig
    libGL
  ];

  nativeBuildInputs = [
    pkg-config
    qt6.wrapQtAppsHook
  ];

  # There are no tests in the crate.
  doCheck = false;

  meta = {
    description = "Viewer for .slint files (pinned to 1.17.0 to match the workspace slint)";
    mainProgram = "slint-viewer";
    homepage = "https://crates.io/crates/slint-viewer";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
})
