# Known Bugs & Issues

## Open

- [GUI power-state gate misses the wire's new power-state spellings](bugs/power-state-vocabulary-drift.md)
- [Per-field config-validation error routing is dead in the GUI](bugs/edit-pane-field-error-routing-dead.md)
- [Session event handler `last_action` cache survives daemon reconnects](bugs/last-action-cache-survives-reconnect.md)
- [`run_pairing_handshake` has no timeout on `wait_for_completion`](bugs/pair-tv-no-timeout.md)

## Closed

- [#21 — Newly paired TV has zeroed D-Bus properties until next subscription update](https://github.com/ruffsl/TisVlay/issues/21)
- [#22 — TV ID / D-Bus path inconsistency](https://github.com/ruffsl/TisVlay/issues/22) — fixed in [#20](https://github.com/ruffsl/TisVlay/pull/20)
- `RemoveTv` derived TV ID from D-Bus path — resolved by moving `Remove` to per-object `Tv1.Remove()` (`2837098`); the handler now reads `&self.tv_id` directly so there's no path-decoding step to be fragile about.
