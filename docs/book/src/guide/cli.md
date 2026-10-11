# CLI Reference

`tisvlay` is a pure client of the daemon — every verb below is a one-shot command against the daemon's control socket. Most verbs that act on a TV take its **UUID** as the first argument; get it from `tisvlay list`. The global options (socket paths, `--json`) are listed under [`tisvlay`](#tisvlay) below.

To see what's going on:

- `tisvlay list` — every TV and its current state (connection, power, input, …).
- `tisvlay status` — a quick check that the daemon itself is up.
- `tisvlay watch` — streams every state key (history first, then live) — the go-to for scripting and for watching automation happen.

**Tab completion** ships for bash, zsh, and fish (installed by the packages; see [Installation](../getting-started/installation.md)). It's live: `<TAB>` completes not just verbs and flags but TV UUIDs, profile UUIDs, and vendor ids, queried from the running daemon and annotated with their names — so you never type a UUID by hand.

The full command tree below is generated from the `tisvlay` source, so it always matches `--help`.

<!-- Generated from the `tisvlay` clap definitions by `just docs-gen`
     (crates/tisvlay-docgen); don't edit the partial by hand. -->
{{#include ../generated/cli.md}}
