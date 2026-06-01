//! Renders the `tisvlay` clap command tree to Markdown for the mdBook
//! user docs. Output is the include-partial
//! `docs/book/src/guide/cli-generated.md` (committed; refreshed via
//! `just docs-cli`, drift-gated by `just docs-cli-check`).
//!
//! The partial is included into `docs/book/src/guide/cli.md`; its own
//! top-level heading is the CLI Reference page's title.
//!
//! Run: `cargo run -p tisvlay-cli --example gen-cli-docs`
//! (prints to stdout; the justfile redirects it to the partial).

fn main() {
    print!("{}", clap_markdown::help_markdown::<tisvlay_cli::Cli>());
}
