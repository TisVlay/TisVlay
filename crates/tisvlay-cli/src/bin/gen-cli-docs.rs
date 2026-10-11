//! Renders the `tisvlay` clap command tree to Markdown for the mdBook
//! user docs. Output is the include-partial
//! `docs/book/src/guide/cli-generated.md` (committed; refreshed via
//! `just docs-cli`, drift-gated by `just docs-cli-check`).
//!
//! The partial is included into `docs/book/src/guide/cli.md`, which
//! supplies the page's own `# CLI Reference` heading and intro. clap-markdown
//! always emits its own H1 title and a boilerplate preamble, so this strips
//! that leading run (and the tool's footer) to leave a single heading on the
//! rendered page.
//!
//! Gated behind the `gen-docs` feature so building it pulls only the CLI
//! (not the dev-dependency graph), keeping the docs CI job lightweight.
//! Run: `cargo run -p tisvlay-cli --bin gen-cli-docs --features gen-docs`
//! (prints to stdout; the justfile redirects it to the partial).

#![expect(clippy::print_stdout, reason = "the generator's output is its stdout")]

fn main() {
    let markdown = clap_markdown::help_markdown_custom::<tisvlay_cli::Cli>(
        &clap_markdown::MarkdownOptions::new().show_footer(false),
    );
    // Drop the leading title / blank / preamble lines so the partial begins
    // at the "Command Overview" that the page's own heading introduces.
    // Per-line trim: clap-markdown leaves a trailing space after the em
    // dash of an undocumented subcommand, which the repo's whitespace hook
    // would strip right back — emit hook-clean output so the committed
    // partial and a fresh regeneration always agree.
    let body: String = markdown
        .lines()
        .skip_while(|line| {
            line.trim().is_empty()
                || line.starts_with("# ")
                || line.starts_with("This document contains the help content")
        })
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    println!("{}", body.trim_end());
}
