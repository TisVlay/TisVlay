## CLI `pair` gets no vendor schema defaults

**Priority**: low
**Found by**: mdBook user-guide reconciliation (writing the mock triage docs)

### Context

`tisvlay pair <name> --vendor <v> --params '<json>'` sends its `--params` object as the pairing config, and the daemon's `pair_tv` passes that through to the connector verbatim, merging **no** defaults from the vendor's `config_schema()`. Any field can be carried, but every field whose default matters has to be spelled out.

The concrete victim is the mock backend: its `pairing` shorthand serde-defaults to `"manual"`, which waits for a `MockHandle` call that only exists in tests (`new_production_parts` deliberately creates no handle). So `tisvlay pair <name> --vendor mock --params '{"address":"mock://x"}'` hangs in production; the mock health check in the [troubleshooting page](../book/src/operations/troubleshooting.md) passes `"pairing":"accept"` explicitly for that reason. The GUI is unaffected: the wizard's schema-driven form prefills the schema default (`"accept"`).

### Risk

- A future vendor with a pairing field whose schema default matters (PIN entry mode, port, serial baud, …) would need it spelled out in every CLI `--params`, where the GUI fills it in. The Epson backend draft is the first candidate to test this.
- Field users who pair a mock from the CLI without `"pairing":"accept"` hit a silent hang — the worst failure shape.

### Mitigation options

- **Daemon-side schema-default merge** (preferred): before `start_pairing`, fill absent config keys from the connector's `config_schema()` defaults. Fixes every client at once and makes the schema's `default` field load-bearing instead of GUI-only; the mock's `accept` default then applies to the CLI too.
- **CLI-side schema fill**: `pair` fetches the vendor's schema (`tisvlay vendor <v>` already does) and fills absent `--params` keys from its defaults. Fixes the CLI only; the daemon-side merge is the better home.
- **Mock-side only**: flip the mock's serde default to `accept` and have tests pass `pairing = "manual"` explicitly. Narrowest fix; touches many test fixtures and doesn't help future vendors.

### Re-evaluation triggers

- A second vendor's pairing schema has a field whose default matters (e.g. the Epson backend leaving draft).
- A user reports the CLI mock-pair hang in the field.
