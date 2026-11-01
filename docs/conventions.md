# Code conventions

<!-- Imported by .claude/CLAUDE.md, so every line here costs context in each Claude session. Keep it short. -->

These are the rules the lints can't fully express. The lint policy and its reasons are in [build.md § Lint policy](build.md#lint-policy).

- **Panics state their invariant.** `expect("… because …")`, never a bare `unwrap()` in shipped code. Tests may unwrap.
- **Errors:** published crates and the client SDK expose `thiserror` types callers can match on. Binaries and binary-glue libraries (`tisvlay-runtime`, the service crates' library halves) may use `anyhow`. The model is in [architecture.md § Error model](architecture.md#error-model).
- **Logging:** through `tracing`, with structured fields (`info!(%tv, "connected")`) rather than values formatted into the message. No `println!`/`eprintln!` outside the CLI (lint-enforced).
- **Secrets never reach `Debug`.** A credential field is a `tisvlay_core::Secret`: its `Debug` prints `[REDACTED]`, serde sees a plain string, and the value is read through `expose()`. Untyped vendor config is a `tisvlay_core::VendorConfig`, and a wire request carrying vendor config hand-writes `Debug` to print key names only.
- **Handles whose drop cancels work** (watch handles, the service stacks, liveliness guards) carry `#[must_use = "…"]` saying what dropping them does.
- **Published crates** (core, protocol, and the `tisvlay` SDK) keep items private by default. `#[non_exhaustive]`, public-dependency and README rules are in [build.md § Public API policy](build.md#public-api-policy); `just package-check` packages them.
- **A wire-visible change is a `feat` or a breaking commit, never a `fix`:** any change to a key, payload, rpc or decode rule. Clients and services check each other's release on `major.minor`, and release-plz bumps the minor only for those commit types, so a protocol change shipped as a patch would pass the check and fail at runtime.
- **A new rpc is declared once** in its family's table in `tisvlay-protocol`'s `endpoint` module (key, request, response, read or write); the compiler then demands the server arm. A world-answered rpc also gets a `Command` variant, and the reactor's exhaustive match then demands its one-shot handler in `tisvlay-world`'s `systems/commands.rs`. Add the SDK or CLI surface by hand. Requests decode strictly: no compatibility shims on the wire.
- **A new per-TV attribute in the CLI** is a getter/setter verb, `<attribute> <tv> [<value>]`, with `--toggle`/`--clear` where they apply: no `set-` prefix, `on|off` for switches, and the bare form reads the value. No aliases for renamed verbs.
- **Allowances:** reasoned `#[expect(lint, reason = "…")]`. Use `#[allow]` with a reason only where the lint depends on features or tests.
- **`unsafe`:** only in files that carry a reasoned `#![expect(unsafe_code)]`, one operation per block, each with a `// SAFETY:` comment.
- **Spelling and wrapping:** American English, which the typos gate enforces. Markdown isn't hard-wrapped: one paragraph per line.
