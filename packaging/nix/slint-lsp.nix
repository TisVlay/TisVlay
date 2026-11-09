# `slint-lsp` at the workspace's Slint version, for the editor
# integrations' live preview of the UI: the LSP reads the crate's
# `slint-project.json` edition, which nixpkgs' older release doesn't know.
# nixpkgs' own recipe (pkgs/by-name/sl/slint-lsp) with the Slint
# repository as its source while 1.19 is a pre-release (flake.nix's
# `slintSrc`); back to nixpkgs' package once it ships the release.
{
  slint-lsp,
  slintSrc,
  slintCargoDeps,
}:
slint-lsp.overrideAttrs (_: {
  version = "1.19.0";
  src = slintSrc;
  cargoDeps = slintCargoDeps;
  buildAndTestSubdir = "tools/lsp";
})
