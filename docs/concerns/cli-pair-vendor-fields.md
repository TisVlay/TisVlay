## CLI `pair` can't carry vendor-specific fields

**Priority**: low
**Found by**: mdBook user-guide reconciliation (writing the mock triage docs)

### Context

`tisvlay pair <address> --vendor <v>` builds its pairing config from exactly two generic fields — `build_pairing_config_json` in `crates/tisvlay-cli/src/main.rs` sends `{address, wake_id?}` — and the daemon's `pair_tv` passes that through to the connector verbatim, merging **no** defaults from the vendor's `config_schema()`. Any pairing-relevant field beyond address/wake-id is therefore unreachable from the CLI.

The first concrete victim is the mock backend: its `pairing` shorthand serde-defaults to `"manual"`, which waits for a `MockHandle` call that only exists in tests (`new_production_parts` deliberately creates no handle). So `tisvlay pair mock://x --vendor mock` hangs in production. The GUI is unaffected — the wizard's schema-driven form prefills the schema default (`"accept"`) — and the user guide documents the GUI and config-file routes ([troubleshooting page](../book/src/operations/troubleshooting.md)), so nothing user-facing is broken, just asymmetric.

### Risk

- A future vendor whose pairing genuinely needs a non-address field (PIN entry mode, port, serial baud, …) would be CLI-unpairable, forcing GUI-or-hand-edit. The Epson backend draft is the first candidate to test this.
- Field users following CLI-centric debugging instructions may try `pair --vendor mock` and hit a silent hang — the worst failure shape.

### Mitigation options

- **Daemon-side schema-default merge** (preferred): before `start_pairing`, fill absent config keys from the connector's `config_schema()` defaults. Fixes every client at once and makes the schema's `default` field load-bearing instead of GUI-only; the mock's `accept` default then applies to the CLI too.
- **CLI-side generic field flag**: a repeatable `--set key=value` on `pair` for vendor fields. More typing but fully general; shares design space with the deferred [CLI shorthand for `Tv1.UpdateConfig`](../ideas/cli-update-config-shorthand.md) idea.
- **Mock-side only**: flip the mock's serde default to `accept` and have tests pass `pairing = "manual"` explicitly. Narrowest fix; touches many test fixtures and doesn't help future vendors.

### Re-evaluation triggers

- A second vendor's pairing schema needs a non-address field (e.g. the Epson backend leaving draft).
- A user reports the CLI mock-pair hang in the field.
- The `Tv1.UpdateConfig` CLI-shorthand idea gets picked up — the `--set` flag design should be shared, not invented twice.
