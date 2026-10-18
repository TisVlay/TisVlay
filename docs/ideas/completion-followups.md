# Shell-completion follow-ups

Deferred work parked when the `tisvlay` shell-completion feature merged. The feature ships clap_complete's dynamic engine with live value completion for TV UUIDs (device verbs), system/user profile UUIDs (the `profile system` / `profile user` subcommands and the `system-profile` / `user-profile` verbs), and vendor ids (the `--vendor` args + `vendor`), all behind a shared time-bounded, empty-on-failure query. See the [packaging README's "Shell completions"](../../packaging/README.md#shell-completions) section for the as-built reference.

## Latency & UX

- **`TISVLAY_COMPLETE_NO_DAEMON` kill switch.** Every value-completing `<TAB>` spawns a daemon round-trip. `COMPLETE=0` disables completion outright, but there is no way to keep subcommand/flag completion while dropping only the live query. An env knob that makes the candidate helpers return empty immediately would serve a user on a slow or flaky socket who still wants structural completion. Trigger: a completion-latency complaint.
- **Short-TTL UUID cache.** Each `<TAB>` is a fresh process, so nothing is reused across rapid successive tabs — the ~180 ms hydrate repeats every time. A short-TTL cache under `$XDG_RUNTIME_DIR/tisvlay/` would make repeat tabs instant and let a momentary daemon blip fall back to last-known instead of empty (strictly better than the current empty-on-timeout). Trigger: the per-TAB latency proves annoying in practice.

## Honoring non-default targets

- **Let completion see `--socket`/`--site`/`--user-socket`.** The completers run before clap parses argv (a `CompleteEnv` constraint), so they can't read the global flags and always resolve the default sockets + the `main` site. Introducing `TISVLAY_SOCKET` / `TISVLAY_SITE` env vars honored by both the CLI and the completers would let a multi-site or non-default-socket user get live candidates for the right daemon. Trigger: a user running a non-default site/socket reports empty completion.

## Value coverage

- **Input IDs for `input` and `primary-input`.** A TV's `state/input` lists its inputs, so `<TAB>` could offer them, but the completers run before clap parses argv and can't see which TV the command names; offering every TV's inputs at once would mislead. Needs a completer that receives the already-typed arguments (or reads them from the raw argv the engine passes). Trigger: clap_complete gains context-aware completers, or a user asks for it.
