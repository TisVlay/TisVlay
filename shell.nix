# `nix-shell`: the contributor dev shell, the same one `nix develop` gives.
# Takes default.nix's arguments, e.g. `nix-shell --argstr system aarch64-linux`.
{ ... }@args: (import ./default.nix args).shell
