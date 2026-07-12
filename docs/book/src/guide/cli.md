# CLI Reference

`tisvlay` is a pure client of the daemon — every verb below is a one-shot command against the daemon's control socket. Most verbs that act on a TV take its **UUID** as the first argument; get it from `tisvlay list`.

A few global flags apply to any verb:

- `--socket <path>` — the daemon socket (default `$XDG_RUNTIME_DIR/tisvlay/zenoh.sock`, falling back to the packaged `/run/tisvlay/zenoh.sock`).
- `--user-socket <path>` — the session service's socket, used by the `user-profile` and `remote-session` verbs.
- `--site <name>` — the site key namespace (default `main`); leave it unless you run more than one daemon.
- `--json` — machine-readable output on read verbs.

`tisvlay watch` streams every state key (history first, then live) — the go-to for scripting and for watching automation happen.

The full command tree below is generated from the `tisvlay` source, so it always matches `--help`.

<!-- The reference below is generated from the `tisvlay` clap definitions
     by `just docs-cli` (see crates/tisvlay-cli/src/bin/gen-cli-docs.rs).
     Do not edit cli-generated.md by hand. -->
{{#include cli-generated.md}}
