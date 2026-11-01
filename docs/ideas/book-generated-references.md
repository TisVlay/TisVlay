# More generated references in the book

The user guide already generates what lives in code: the CLI reference, the profile defaults and the release version, into `docs/book/src/generated/` behind the `just docs-gen-check` drift gate. Two more surfaces could work the same way.

## NixOS module options

Option names and defaults are consolidated by hand in Installation's NixOS section, and a couple of other pages quote the one they need. A generated page (`nixosOptionsDoc`) would own them from `packaging/nix/module.nix`, and the prose would name an option and link to it.

The drift gate is the catch: `ci.yml` doesn't use Nix, so checking this page needs a Nix step there.

**Trigger:** the option surface keeps growing, or `ci.yml` gains a Nix step anyway.

## Per-vendor pairing fields and capabilities

The vendor catalog (`tisvlay vendor <vendor>`) could generate the book's per-backend tables.

**Trigger:** a second network backend exists. One LG row doesn't justify a generator.
