# Shell-completion follow-ups

Deferred work parked when the `tisvlay` shell-completion feature merged. The feature ships clap_complete's dynamic engine with live value completion for TV UUIDs (device verbs) and system/user profile UUIDs (the `profile` / `user-profile` subcommands + `assign`), all behind a shared time-bounded, empty-on-failure query. See the [packaging README's "Shell completions"](../../packaging/README.md#shell-completions) section for the as-built reference.

## More value sources

- **Vendor-name completion for `--vendor`.** The `--vendor` flag on `discover`, `discover-session`, `pair`, and `add` takes a free-form vendor id. The vendor catalog is already queryable (`keys::vendors`), so an `ArgValueCandidates` source could offer `lg`/`cec`/`mock`/… described by display name — the same shape as the profile catalogs, minus the per-TV hydrate. Trigger: a user fumbles a vendor id, or the registered vendor set grows.

## Latency & UX

- **`TISVLAY_COMPLETE_NO_DAEMON` kill switch.** Every value-completing `<TAB>` spawns a daemon round-trip. `COMPLETE=0` disables completion outright, but there is no way to keep subcommand/flag completion while dropping only the live query. An env knob that makes the candidate helpers return empty immediately would serve a user on a slow or flaky socket who still wants structural completion. Trigger: a completion-latency complaint.
- **Short-TTL UUID cache.** Each `<TAB>` is a fresh process, so nothing is reused across rapid successive tabs — the ~180 ms hydrate repeats every time. A short-TTL cache under `$XDG_RUNTIME_DIR/tisvlay/` would make repeat tabs instant and let a momentary daemon blip fall back to last-known instead of empty (strictly better than the current empty-on-timeout). Trigger: the per-TAB latency proves annoying in practice.

## Honoring non-default targets

- **Let completion see `--socket`/`--site`/`--user-socket`.** The completers run before clap parses argv (a `CompleteEnv` constraint), so they can't read the global flags and always resolve the default sockets + the `main` site. Introducing `TISVLAY_SOCKET` / `TISVLAY_SITE` env vars honored by both the CLI and the completers would let a multi-site or non-default-socket user get live candidates for the right daemon. Trigger: a user running a non-default site/socket reports empty completion.
