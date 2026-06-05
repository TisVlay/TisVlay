# `a{sv}` wire-format migration for vendor-config surfaces

`Manager1.AddTv` and `Tv1.UpdateConfig` currently take `config_json: s` (JSON string). The architecturally correct end-state per freedesktop convention is `a{sv}` (D-Bus variant dict), matching ModemManager's `Modem.Simple.Connect(properties: a{sv})` and `ProfileManager.Set(properties: a{sv})` shape.

Triggers for reconsidering:

1. A second vendor backend lands and the multi-vendor introspection argument becomes concrete.
2. External tools (third-party clients, monitoring scripts) start consuming the D-Bus surface and the typed introspection becomes user-visible.
3. A vendor config grows nested structure that `a{sv}` would handle awkwardly but JSON would handle cleanly (which would *strengthen* the case for staying on JSON, not weaken it).

Cost of locking in `a{sv}` now is migrating `AddTv` callers during peak-development churn for no concrete user-visible benefit. Cost of staying on JSON is a future wire-flip whose blast radius grows with each shipped consumer.

## Decision (second-vendor trigger, recorded inline)

Trigger 1 fired: a second real vendor backend — the Epson projector backend ([docs/design.md § Dependency Graph](../design.md#dependency-graph)) — landed alongside LG and Mock. The decision is to **stay on the JSON `config_json: s` wire form**. The multi-vendor introspection argument is now concrete but stays weak: every vendor's config still travels as the same opaque payload that only its own crate deserializes (the whole point of the [opaque-`toml::Value` design](../backends/implementing-a-vendor.md#why-vendor-config-is-an-opaque-tomlvalue-not-a-core-known-enum)), so typed `a{sv}` introspection buys no per-vendor field visibility that the schema (`GetPairingSchema` / `config_schema()`) doesn't already provide more richly. One more vendor doesn't change the blast-radius calculus — it adds zero shipped *external* D-Bus consumers (trigger 2), which is the cost that actually grows with a JSON-to-`a{sv}` flip. Migrating `AddTv` / `UpdateConfig` callers now would be churn for no user-visible benefit. Revisit on trigger 2 (an external tool starts consuming the surface) or trigger 3 (a vendor config grows nested structure — which, per above, *strengthens* the JSON case rather than weakening it).
