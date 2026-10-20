# Two binaries: `tisvlay` (CLI + services) and `tisvlay-gui`

TisVlay ships six binaries, and each statically links its own copy of the shared stack: zenoh, tokio, zbus, the SDK and `std`. Folding the CLI and the four headless services into one `tisvlay` binary removes most of that duplication. The GUI stays separate, because it alone links Slint and the system graphics libraries.

## Why

Measured with `cargo bloat --crates` on unstripped `release` builds (2026-10-20, x86_64). Text is the machine code; each crate counts once, at its largest, when merged.

| Binary | File | Text |
|---|---|---|
| `tisvlay` (CLI) | 24 MiB | 9.1 MiB |
| `tisvlay-daemon` | 41 MiB | 16.1 MiB |
| `tisvlay-session` | 30 MiB | 12.0 MiB |
| `tisvlay-tray` | 30 MiB | 11.7 MiB |
| `tisvlay-greeter` | 26 MiB | 9.6 MiB |
| `tisvlay-gui` | 61 MiB | 25.1 MiB |

- **CLI + four services merged:** 58.6 MiB of text summed, about 21 MiB merged (−64%).
- **All six merged:** 83.8 → about 35 MiB (−58%). That's barely more than the two-binary split saves, at the cost below.
- **What repeats:** zenoh's crates alone come to about 4 MiB of text in every binary, plus zbus (1.2–1.7 MiB) and `std` (about 2 MiB).

These are estimates: monomorphized generics don't merge perfectly. A prototype build gives the real number. The savings stack with the `dist` profile, which already brings the six from 126 to 71 MiB (Cargo.toml has the breakdown).

**Why the GUI stays out:** the five headless binaries link only libc. The GUI also links fontconfig, freetype, libpng, expat, brotli, bzip2 and zlib. Merged, every CLI call would load those libraries, and the CLI binary would carry Slint.

## Design

- **Verbs:** `tisvlay service daemon|session|tray|greeter [args]`, with each service's current flags unchanged under its verb. Each service crate's `main.rs` becomes a library `run(args)` that the CLI calls.
- **Visible, under its own heading:** the `service` group appears in top-level `--help`, listed last under a "Services" heading ("run a TisVlay service, normally started by systemd"), in the shell completions and in the generated CLI reference. That's one extra line in the everyday help. People do run the services by hand: during development, and an admin adding daemon flags through the module's `extraArgs` needs `tisvlay service daemon --help`. Comparable single-binary tools keep their server verbs visible too: `consul agent`, `vault server`, `gitea web`, `caddy run`, `k3s server`. Hiding is for modes only the program itself starts, like Chromium's `--type=renderer` helper processes.
- **Names in logs:** process names (`comm`) all become `tisvlay`. Each systemd unit sets `SyslogIdentifier=tisvlay-<service>`, so `journalctl -t tisvlay-daemon` keeps working.
- **Old binary names (open):** symlinks such as `tisvlay-daemon → tisvlay`, dispatched on `argv[0]` busybox-style, would keep scripts and process names working. Without them, the switch is a clean break in one release (the project's no-shims rule leans that way).

## Ripple list

- **Crates:** each service crate becomes library-only (or keeps a thin binary for `cargo run` during development; open). `tisvlay-cli` depends on the four service libraries, possibly behind a default `services` feature so a CLI-only build stays possible.
- **systemd:** the units in `packaging/systemd/` and `module.nix`: `ExecStart=…/tisvlay service <name>`, plus `SyslogIdentifier`.
- **Packaging:** the nfpm contents list in `crates/tisvlay-daemon/Cargo.toml` (cargo-nfpm adds the `-p` crate's binary itself, so the packaging entry point moves too). In `package.nix`, the postFixup rpath splice moves from the session and greeter binaries to `tisvlay`, since the session and greeter dlopen libwayland.
- **Dev recipes:** the `*-start` recipes become `cargo run --bin tisvlay -- service <name> …`, and `stop-all`'s `pkill`/`pgrep` patterns can no longer match on binary names. It needs PID files or full command-line patterns.
- **Docs:** architecture.md's topology section, packaging/README.md, the book's operations pages that name binaries, and CLAUDE.md's crate list. The generated CLI reference picks up the `service` group by itself (`just docs-gen`).
- **Tests:** integration tests boot the stacks in-process, so they don't depend on binary names. The CLI's completion test (`CARGO_BIN_EXE_tisvlay`) covers the new group like any other verb.

## To measure first

- [ ] A prototype merge: build `tisvlay` with the four services linked in, compare file size and text against the estimate.
- [ ] CLI startup: `hyperfine 'tisvlay --version'` and `tisvlay status` before and after. With no new dynamic libraries it should stay within noise, but the binary is larger.
- [ ] Development builds: the incremental relink time of the merged binary under the `test` profile, since any service change now relinks it.

## Open questions

- Symlinks for the old names, or a clean break?
- Keep thin per-service binaries for development, or run everything through `tisvlay service`?
- Should the `service` group be a cargo feature, so distributions could ship a CLI-only package?
