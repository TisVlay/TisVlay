# Known Bugs & Issues

- [TV ID / D-Bus path inconsistency](https://github.com/ruffsl/TisVlay/issues/22) — **fixed** in [#20](https://github.com/ruffsl/TisVlay/pull/20)
- [`RemoveTv` still derives TV ID from D-Bus path](bugs/remove-tv-derives-id-from-path.md) — open
- [`TvManager::pair_tv` has no timeout on `wait_for_completion`](bugs/pair-tv-no-timeout.md) — open
- [Session event handler `last_action` cache survives daemon reconnects](bugs/last-action-cache-survives-reconnect.md) — open
- [Newly paired TV has zeroed D-Bus properties until next subscription update](https://github.com/ruffsl/TisVlay/issues/21) — **fixed**
