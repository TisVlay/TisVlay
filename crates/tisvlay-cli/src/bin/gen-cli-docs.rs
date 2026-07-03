//! Renders the `tisvlay` clap command tree to Markdown for the mdBook
//! user docs. Output is the include-partial
//! `docs/book/src/guide/cli-generated.md` (committed; refreshed via
//! `just docs-cli`, drift-gated by `just docs-cli-check`).
//!
//! The partial is included into `docs/book/src/guide/cli.md`; its own
//! top-level heading is the CLI Reference page's title.
//!
//! Gated behind the `gen-docs` feature so building it pulls only the CLI
//! (not the dev-dependency graph), keeping the docs CI job lightweight.
//! Run: `cargo run -p tisvlay-cli --bin gen-cli-docs --features gen-docs`
//! (prints to stdout; the justfile redirects it to the partial).

fn main() {
    print!("{}", clap_markdown::help_markdown::<tisvlay_cli::Cli>());
}
