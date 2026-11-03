# cargo-about 0.9.1, exactly. It's the first release whose generator honors
# LicenseRef-* clarify files (EmbarkStudios/cargo-about#303); an older one
# drops the vendored Slint crates' royalty-free license text from
# THIRD-PARTY-LICENSES.html with only a "no license file" warning, and
# 0.9.2 drops it again the same way. Neither fails the build, so the
# version is pinned rather than floored: bump it only after generating the
# bundle with both versions and checking the Slint crates keep their
# license text.
#
# nixpkgs' copy passes through when it's this version; otherwise this
# builds 0.9.1 from source.
{
  cargo-about,
  fetchFromGitHub,
  rustPlatform,
}:

if cargo-about.version == "0.9.1" then
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
    # Upstream's own suite: a build tool's tests, run on every release
    # build here since this copy isn't in any binary cache.
    doCheck = false;
  }
