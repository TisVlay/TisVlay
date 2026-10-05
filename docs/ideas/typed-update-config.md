# A typed `update-config` request

`tv/<id>/rpc/update-config` takes `rpc::UpdateConfig`, a newtype over a JSON map that mixes the TV's own settings (`name`, `managed`, `system_profile`, `primary_input`) with vendor keys. The world's `apply_config_patch` dispatches on key-name strings: known names apply to settings, `id`/`vendor` are refused as immutable, and everything else merges into the vendor table with `null` unsetting. It is the last stringly-dispatched request after the typed-endpoint work, and the settings half duplicates the dedicated setters (`set-name`, `set-managed`, `set-system-profile`, `set-primary-input`).

## Path forward

Type the request:

```rust
pub struct UpdateConfig {
    pub name: Option<String>,
    pub managed: Option<bool>,
    pub system_profile: Option<Option<Uuid>>,
    pub primary_input: Option<Option<String>>,
    #[serde(flatten)]
    pub vendor: serde_json::Map<String, serde_json::Value>,
}
```

`Option<Option<_>>` keeps "leave alone" apart from "clear" (with `serde_with`'s double-option or a hand-written deserializer). The immutable-key check moves into decoding (`id`/`vendor` in the flattened map are refused), `apply_config_patch` becomes field assignments plus the vendor merge, and the key-names-only `Debug` covers the `vendor` map alone. The GUI's edit-pane save and the CLI's `config-set` build the struct instead of a map. Alternatively, drop the settings half and have the edit pane call the dedicated setters, leaving `update-config` vendor-only.

## Trigger conditions

- The next change to the edit pane's save path, or a new TV-level setting that would otherwise grow the string dispatch.
