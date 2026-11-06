# Adds `tisvlay` to a nixpkgs, built against that nixpkgs with the
# `release` profile. The module's `package` default picks it up.
final: prev: {
  tisvlay = final.callPackage ./package.nix { };
}
