# Known Bugs & Issues

## Open

- [`RemoveTv` still derives TV ID from D-Bus path](bugs/remove-tv-derives-id-from-path.md)
- [Session event handler `last_action` cache survives daemon reconnects](bugs/last-action-cache-survives-reconnect.md)
- [`TvManager::pair_tv` has no timeout on `wait_for_completion`](bugs/pair-tv-no-timeout.md)

## Closed

- [#21 — Newly paired TV has zeroed D-Bus properties until next subscription update](https://github.com/ruffsl/TisVlay/issues/21)
- [#22 — TV ID / D-Bus path inconsistency](https://github.com/ruffsl/TisVlay/issues/22) — fixed in [#20](https://github.com/ruffsl/TisVlay/pull/20)
