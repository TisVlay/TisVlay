# cargo-about at least 0.9.1, the first release whose generator honors
# LicenseRef-* clarify files (EmbarkStudios/cargo-about#303). An older one
# drops the vendored Slint crates' royalty-free license text from
# THIRD-PARTY-LICENSES.html with only a "no license file" warning.
#
# nixpkgs ships 0.9.0, so this builds 0.9.1 from source until nixpkgs
# catches up, and passes nixpkgs' through after that. Delete this file then,
# along with its callers in package.nix and flake.nix's devShell.
{
  lib,
  cargo-about,
  fetchFromGitHub,
  rustPlatform,
}:

if lib.versionAtLeast cargo-about.version "0.9.1" then
  cargo-about
else
  let
    src = fetchFromGitHub {
      owner = "EmbarkStudios";
      repo = "cargo-about";
      tag = "0.9.1";
      hash = "sha256-mZyfECnlpYom/XI445DOeUAthqK3HKC+QkkK+5gf0/E=";
    };
  in
  cargo-about.overrideAttrs {
    version = "0.9.1";
    inherit src;
    cargoDeps = rustPlatform.fetchCargoVendor {
      inherit src;
      hash = "sha256-//t3ASNljC4rosk5oOmcJy8PZKRYjlQ0euzzAmfiSPQ=";
    };
  }
