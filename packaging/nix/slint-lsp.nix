# `slint-lsp` at the workspace's Slint version, for the editor
# integrations' live preview of the UI: an LSP older than the edition in
# the crate's `slint-project.json` can't read the project file. nixpkgs'
# own recipe (pkgs/by-name/sl/slint-lsp) with the Slint repository as its
# source while 1.19 is a pre-release (flake.nix's `slintSrc`); `fetchCrate`
# 1.19.0 once it's published, as slint-viewer.nix does, and the file goes
# when nixpkgs' slint-lsp reaches the workspace's pin.
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
