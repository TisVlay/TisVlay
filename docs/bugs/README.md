# Known Bugs & Issues

## Open

- [Upstream: linux-cec re-claim drops the OSD name](upstream-linux-cec-reclaim-drops-osd-name.md) — draft for linux-cec, not filed; held until our workaround (`8f764d5`) soaks
- [Session event handler `last_action` cache survives daemon reconnects](last-action-cache-survives-reconnect.md)
- [`run_pairing_handshake` has no timeout on `wait_for_completion`](pair-tv-no-timeout.md)

## Closed

- [GUI and tray show a remote-off LG TV as powered on](power-switch-active-standby.md) — all three clients key on the shared `PowerState::is_awake`
- [GUI power-state gate misses the wire's new power-state spellings](power-state-vocabulary-drift.md) — gate now delegates to `PowerState::is_on` via `parse`
- [Per-field config-validation error routing is dead in the GUI](edit-pane-field-error-routing-dead.md) — typed `RpcError` + daemon-side `invalid-field` validation
- [#21 — Newly paired TV has zeroed D-Bus properties until next subscription update](https://github.com/TisVlay/TisVlay/issues/21)
- [#22 — TV ID / D-Bus path inconsistency](https://github.com/TisVlay/TisVlay/issues/22) — fixed in [#20](https://github.com/TisVlay/TisVlay/pull/20)
- `RemoveTv` derived TV ID from D-Bus path — resolved by moving `Remove` to per-object `Tv1.Remove()` (`2837098`); the handler now reads `&self.tv_id` directly so there's no path-decoding step to be fragile about.
