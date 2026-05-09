# `a{sv}` wire-format migration for vendor-config surfaces

`Manager1.AddTv` and `Tv1.UpdateConfig` currently take `config_json: s` (JSON string). The architecturally correct end-state per freedesktop convention is `a{sv}` (D-Bus variant dict), matching ModemManager's `Modem.Simple.Connect(properties: a{sv})` and `ProfileManager.Set(properties: a{sv})` shape.

Triggers for reconsidering:

1. A second vendor backend lands and the multi-vendor introspection argument becomes concrete.
2. External tools (third-party clients, monitoring scripts) start consuming the D-Bus surface and the typed introspection becomes user-visible.
3. A vendor config grows nested structure that `a{sv}` would handle awkwardly but JSON would handle cleanly (which would *strengthen* the case for staying on JSON, not weaken it).

Cost of locking in `a{sv}` now is migrating `AddTv` callers during peak-development churn for no concrete user-visible benefit. Cost of staying on JSON is a future wire-flip whose blast radius grows with each shipped consumer. Revisit when the second vendor backend is in flight.
