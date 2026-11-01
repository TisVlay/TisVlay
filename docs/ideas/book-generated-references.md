# More generated references in the book

The user guide already generates what lives in code: the CLI reference, the profile defaults and the release version, into `docs/book/src/generated/` behind the `just docs-gen-check` drift gate. Two more surfaces could work the same way.

## NixOS module options

Every option mention is consolidated by hand in Installation's NixOS section, and other pages link there. A generated page (`nixosOptionsDoc`) would own the names and defaults from `packaging/nix/module.nix`, and the prose would name an option and link to it.

The drift gate is the catch: CI doesn't use Nix, so checking this page needs a Nix step in CI.

**Trigger:** the option surface keeps growing, or CI gains a Nix step anyway.

## Per-vendor pairing fields and capabilities

The vendor catalog (`tisvlay vendor <vendor>`) could generate the book's per-backend tables.

**Trigger:** a second network backend exists. One LG row doesn't justify a generator.
