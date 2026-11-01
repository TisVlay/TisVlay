# Writing the user guide

The user guide is this mdBook, published on [Read the Docs](https://tisvlay.readthedocs.io/). Everything else under `docs/` is for developers. `just docs-serve` previews it live, and `just docs-links` checks its links and anchors.

## Rules

- **One canonical home per fact.** Elsewhere, keep only the consequence the reader needs at that point, in a clause, plus a link.
- **The CLI reference, profile defaults and release version are generated,** into `src/generated/` behind a drift gate. Don't hand-copy a default or a flag into the prose.
- **Describe tasks with the labels the GUI shows,** taken from `crates/tisvlay-gui/ui/`, and keep implementation vocabulary (UUIDs, sockets, field names) off the main path. When a label changes, grep the book and `.github/` for the old one.
- **Command-line equivalents live in one of two places:** the "Which command does what the app does" table on the Command Line page, or a collapsed "From a terminal" `<details>` aside where a terminal really is the fix.
- **Stay agnostic to vendor and transport.** Vendor specifics go in a per-backend table or an explicit "for LG" clause. A new backend folds into the existing pages rather than getting its own chapter.
- **Leave out what drifts:** counts beside the list they count, third-party menu paths (name the config key instead), tested-model lists, build baselines, and references to retired features.

## Canonical homes

| Fact | Page |
|---|---|
| Profile semantics, inheritance, the automation gate, event meanings | Automation |
| The file schema and reload rules | Configuration Files |
| Stream behavior | Streaming |
| Tray and window behavior | GUI & Tray |
| The Wake-on-LAN checklist | Troubleshooting |
| The MAC explanation | Adding a TV |
| Socket access, NixOS options, CEC adapters | Installation |
| Reliability behavior | Networking & Reliability |
| Backend limitations | Requirements & Compatibility |
| The discovery port | Firewall & Discovery |
| Checking a release (checksums, signatures, SBOMs, scanning an installed system) | Security & Supply Chain |

## Tooling

- **Why mdBook:** it's a single Rust binary in nixpkgs, with no Python toolchain to add. Reconsider it if the guide ever needs versioned or translated docs.
- **A new preprocessor** has to be installed in three places: `flake.nix`, the `docs` job in `.github/workflows/ci.yml`, and `.config/.readthedocs.yaml`. Keep their versions in step.
- **The release version partial** comes from the workspace `Cargo.toml`: `just release-bump` regenerates it with the bump, and `just docs-gen-check` fails if it's stale. It assumes `main`'s version equals the latest release, which is untrue from a release PR's merge until that release is published.
