# User-facing documentation site: mdBook → Read the Docs

## Context

tisvlay has no user-facing docs site today — `docs/*.md` is ~95% developer-facing (design, testing, ideas, plans), and the `main` README is a 67-line stub. We want a published user guide (install / pair / CLI / config / firewall / troubleshooting) on **Read the Docs**.

After research comparing the two RTD-supported generators, decisions (made with the user):
- **Tool: mdBook.** Rust-native single binary, in nixpkgs (zero Python), mature/stable (`book.toml`), first-class RTD support — fits the existing Rust + Nix dev shell. Zensical (the Material/MkDocs successor) is feature-rich but **early alpha (v0.0.43)** and pulls a Python toolchain into a Rust+Nix repo; revisit at 1.0 if versioned/i18n docs are ever needed.
- **v1 scope: skeleton + key pages.** Working site + full nav; write the high-value pages (install, quickstart, pairing, CLI reference, configuration, firewall, troubleshooting); stub the rest (profiles deep-dive, session automation, GUI/tray, FAQ).
- **CLI reference: auto-generated from clap** (`clap-markdown`), so it never drifts from `--help`.

**Content seed — the `origin/docs` branch.** An earlier effort (`github.com/ruffsl/TisVlay/tree/docs`, 19 commits, ~45 behind the merge-base) rewrote the README into a **566-line end-user guide** whose sections map almost 1:1 onto the mdBook nav, plus an LG backend README and a system-tray screenshot (`docs/screenshots/system_tray.png` via `.gitattributes` LFS). It is **not** an SSG setup — pure markdown content. Mine it as the primary source for most pages, **reconciling against current `main`** (it predates `on_dpms_off`/`on_dpms_on`, the DPMS/action naming, IPv6, the recent refactors). Read it with `git show docs:README.md` and `git show docs:crates/tisvlay-lg/README.md`.

The mdBook lives at **`docs/book/`** (`book.toml` + `src/`), namespaced so it never collides with the developer `docs/*.md`.

## Implementation

### 1. CLI crate: expose `Cli` + add the markdown generator
`crates/tisvlay-cli` is binary-only (`Cli`/`Commands` are private in `main.rs`). To render the clap tree, move the clap **type definitions** into a new `crates/tisvlay-cli/src/lib.rs` (`pub struct Cli` + `pub` subcommand/value enums), slim `main.rs` to `use tisvlay_cli::{…}` (runtime helpers + match arms stay in `main.rs`), and add `[lib] name = "tisvlay_cli"` alongside the existing `[[bin]]`. The integration test `crates/tisvlay-cli/tests/pair_snapshot.rs` spawns the **binary** (`CARGO_BIN_EXE_tisvlay`), so its 5 snapshots are unaffected as long as the bin still emits identical output.
- Add `clap-markdown = "0.1"` to `[dev-dependencies]` (dev-dep + example → never ships in the release binary).
- New `crates/tisvlay-cli/examples/gen-cli-docs.rs`: `print!("{}", clap_markdown::help_markdown_custom::<tisvlay_cli::Cli>(&opts))` with the generated title suppressed (avoids a double-H1 under the page's own heading).

### 2. mdBook scaffold — `docs/book/`
- `book.toml`: title `tisvlay`, `src = "src"`, `default-theme = "navy"`, search enabled, `git-repository-url` + `edit-url-template = ".../edit/main/docs/book/{path}"` (the `docs/book/` prefix is required since the book root isn't the repo root). Pull the repo URL from `[workspace.package].repository`.
- Enable **`mdbook-mermaid`** in v1 (the Architecture page reuses the `docs` branch's mermaid diagram; plugin is in nixpkgs). **Defer `mdbook-admonish`** to a later iteration to keep the 3-environment install burden (Nix shell / CI / RTD) minimal.
- `src/SUMMARY.md` nav:
  - `[Introduction]` (prefix chapter)
  - **Getting Started**: Installation, Quick Start, Pairing
  - **User Guide**: CLI Reference *(auto)*, Configuration, Profiles *(stub)*, Session Automation *(stub)*, GUI & Tray *(stub)*
  - **Operations**: Firewall & Discovery, Troubleshooting
  - **Reference**: Requirements & Compatibility, FAQ *(stub)*
- Every SUMMARY entry needs a real file (mdBook errors on missing/empty links); stubs get a heading + "Coming soon."

### 3. Wire the auto-generated CLI reference
`just docs-cli` runs the example → writes `docs/book/src/guide/cli-generated.md` (**committed**). The hand-written `src/guide/cli.md` adds intro/examples then `{{#include cli-generated.md}}` (relative to the including file; the partial is not a SUMMARY chapter). RTD runs a **plain `mdbook build`** and never invokes cargo — freshness is enforced in CI instead.

### 4. Content pages (adapt from `origin/docs` README, reconcile vs main)
Map the `docs` branch README sections onto pages: Features/Supported Devices/Architecture(+mermaid) → Introduction; Install (Debian/Ubuntu, Fedora/RHEL, Bazzite/Silverblue rpm-ostree, Arch, Nix flake, Cargo) → Installation; Pairing + day-to-day control → Quick Start + Pairing; Profiles/resolution/options/action-vocabulary/example-configs → Configuration (+ Profiles stub); firewall recipes → adapt `docs/firewall.md` into Operations/Firewall & Discovery; `docs/diagnostics.md` → Troubleshooting; Requirements/Limitations → Reference/Requirements. **Reconcile every config/CLI mention against current `main`** (current clap surface — the CLI page is auto-gen so it's self-correcting; current `SystemProfileConfig`/`UserProfileConfig` fields incl. `on_dpms_off`/`on_dpms_on`; current packaging artifact names). Reuse the tray screenshot on the GUI & Tray page (carry over the `.gitattributes` LFS rule for `docs/screenshots/`).

### 5. `.readthedocs.yaml` (repo root)
```yaml
version: 2
build:
  os: ubuntu-lts-latest
  tools:
    rust: latest
  commands:
    - cargo install mdbook --version 0.5.x --locked
    - cargo install mdbook-mermaid --version <pin> --locked
    - mdbook-mermaid install docs/book
    - mdbook build docs/book --dest-dir $READTHEDOCS_OUTPUT/html
```
Pin mdbook to the **0.5.x** line that nixpkgs ships (avoid local-vs-RTD skew; cold `cargo install` has no cross-build cache, so pinning bounds build time). The RTD Addons flyout still injects under `build.commands` (the "build.commands disables the flyout" note is outdated).

### 6. Tooling integration
- **`.gitignore`**: add `/docs/book/book/` (mdBook output is **not** currently ignored — would flood `git status`).
- **`flake.nix`**: add `mdbook` + `mdbook-mermaid` to the dev-shell `packages = with pkgs; [ … ]` list (lines ~52-99). All in nixpkgs (`mdbook` 0.5.2, `mdbook-mermaid` 0.17.0).
- **`justfile`** — new `[group('docs')]`: `docs-build` (`mdbook build docs/book`), `docs-serve` (`mdbook serve docs/book --open`), `docs-cli` (regen the partial; `@echo "… review and commit."` per the `i18n-extract` idiom), `docs-cli-check: docs-cli` then `git diff --exit-code` (drift gate). Bare commands (no embedded `nix develop -c`), matching `build`/`test`.
- **CI** (`.github/workflows/ci.yml`, which does **not** use Nix): a **separate `docs` job** — `taiki-e/install-action` for `mdbook` + `just`, then `just docs-cli-check` (fails if a clap change wasn't re-generated) and `just docs-build`. Keep it off the tuned `ci` job's cache/critical path.

### 7. README disposition
Keep `main`'s README **concise** (features + quickstart + a prominent link to the docs site); the comprehensive content from the `docs` branch lands in the book rather than re-bloating the README. (Recommendation — adjust at execution if you'd rather keep a fuller README too.)

## Critical files
- New: `crates/tisvlay-cli/src/lib.rs`, `crates/tisvlay-cli/examples/gen-cli-docs.rs`, `docs/book/book.toml`, `docs/book/src/SUMMARY.md` + chapter files, `.readthedocs.yaml`
- Edit: `crates/tisvlay-cli/src/main.rs`, `crates/tisvlay-cli/Cargo.toml`, `justfile`, `flake.nix`, `.github/workflows/ci.yml`, `.gitignore`, `README.md`
- Reference (read-only content source): `git show docs:README.md`, `git show docs:crates/tisvlay-lg/README.md`, `docs/firewall.md`, `docs/diagnostics.md`, `docs/configs/*.toml`, current `crates/tisvlay-core/src/config.rs`

## Gotchas (verified)
- `{{#include}}` paths are relative to the including file.
- `clap-markdown` emits its own H1 + preamble + command-overview TOC — suppress the title via `help_markdown_custom` to avoid a double-H1; the deep `profile user set` tree nests several heading levels (fine for reference).
- `edit-url-template` must carry the `docs/book/` prefix or "Edit this page" 404s.
- Local mdbook (nixpkgs 0.5.x) vs RTD (`cargo install`) version skew — pin both to the same 0.5.x.
- The lib refactor: only type **definitions** move; verify `propagate_version`/`global`/`default_value_t` attributes survive verbatim, then `just build` + `just test` (snapshots green).

## Verification
1. `nix develop -c just docs-cli` then `nix develop -c just docs-build` → site builds into `docs/book/book/`; open `index.html`, confirm nav, search, the included CLI reference, and the mermaid architecture diagram render.
2. `nix develop -c just docs-cli-check` → clean (committed partial matches the clap tree).
3. `nix develop -c just check` → the cli lib refactor keeps the workspace + `pair_snapshot` tests green.
4. `nix develop -c just docs-serve` for live-reload authoring while filling pages.
5. Connect the repo on Read the Docs, point at `.readthedocs.yaml`, confirm a successful cloud build + the Addons flyout.
