# `slint-viewer` at the workspace's Slint version (root Cargo.toml
# [workspace.dependencies] → the exact `slint` pin), so its headless
# `--check` / `--screenshot` renders compile the UI with the compiler the
# app builds with. nixpkgs' is behind the workspace. While 1.19 is a
# pre-release the source is the Slint repository at the revision the
# workspace is patched to (flake.nix's `slintSrc`); back to `fetchCrate`
# once 1.19.0 is published. Mirrors the nixpkgs recipe
# (pkgs/by-name/sl/slint-viewer).
{
  lib,
  rustPlatform,
  slintSrc,
  slintCargoDeps,

  fontconfig,
  libGL,
  pkg-config,
  qt6,
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "slint-viewer";
  version = "1.19.0";

  src = slintSrc;
  cargoDeps = slintCargoDeps;
  buildAndTestSubdir = "tools/viewer";

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
    description = "Viewer for .slint files (at the workspace's slint version)";
    mainProgram = "slint-viewer";
    homepage = "https://crates.io/crates/slint-viewer";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
})
