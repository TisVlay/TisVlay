# Prune core's unused error vocabularies

`tisvlay-core`'s `errors` module still carries vocabularies from the D-Bus stack, where each variant's id became a D-Bus error name. Since the rewrite, rpc refusals are `tisvlay-wire`'s `WireError` with its typed `ErrorCode` (`docs/architecture.md` § Error model), and three of core's types have no users outside the module:

- **`PowerError` and `PowerErrorCode`** (`errors/power.rs`): the `power.*` ids. Power failures reach clients as `WireError` refusals or the `event/dispatch-failed` key instead.
- **`ConfigErrorCode`** (`errors/config.rs`): the `config.*` ids. `ConfigError` itself stays; pairing-schema validation returns it and the GUI's field validator matches `InvalidVendorConfig`.

`DeviceErrorCode` stays too: it's the `device.permission-denied` code on the pairing-progress key.

## Path forward

Delete the three types and their re-exports, and rewrite the module doc: it describes a per-domain, ModemManager-style layout whose ids no longer reach any wire. Core is published, so this is a breaking change to land with the next minor version bump.

## Trigger conditions

- Core's next breaking release, or the typed pairing-progress codes in [typed-failed-reason-payload.md](typed-failed-reason-payload.md), which would rework this module anyway.
