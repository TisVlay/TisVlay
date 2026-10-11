# Per-field config-validation error routing is dead in the GUI

**Status:** closed (2026-07-04). The SDK surfaces rpc failures as a typed `RpcError { code, message }` in the anyhow chain; the daemon validates required vendor fields against the pairing schema (`invalid-field` code, `"<field>: <reason>"` message) on add-tv/pair/update-config; the edit pane and wizard matchers route on the code.

`tisvlay-gui-app/src/edit_pane.rs` (and the wizard's error path) downcasts errors looking for a `zbus::Error::MethodError` named `org.tisvlay.Error.ConfigInvalidVendorConfig` to highlight the offending vendor-config field. The SDK's rpc layer surfaces failures as `anyhow!("{message} ({code})")` built from the typed `WireError` — no zbus error ever appears — so the downcast never matches and every validation failure lands on the generic error banner instead of the field highlight.

Fix direction: surface `WireError` structurally from the SDK (a typed error enum or a payload the GUI can parse: `code` + optional `field`/`path`), then re-point the edit pane and wizard matchers at it. The daemon side already sends namespaced codes; only the client-side plumbing lost its type on the fork.
