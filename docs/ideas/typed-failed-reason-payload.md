# Typed `Failed { reason }` payload across daemon error vocabulary

The `PairingState::Failed` wire format is still `(s)` — a single human-readable string carrying the reason. Plan 5 introduced typed `ConfigError::InvalidVendorConfig { field, reason }` for the pairing-form validation case (so the GUI routes per-field errors back to the originating row), but the broader `Failed { reason }` payload across `Connection` / `Pairing` / `Internal` errors stays untyped on the wire.

Path forward:

- Define typed sub-variants (`Failed::ConnectionRefused`, `Failed::TvUnreachable { last_seen_at }`, `Failed::PairingRejected`, `Failed::Internal { kind }`) under the existing `tisvlay-core/src/errors/` per-domain enum tree.
- D-Bus wire encoding: variant-tagged dict rather than a free-form string, so clients can branch on reason kind.
- Migrate `tisvlay` and the GUI's inline-error surface to render the typed variant rather than the raw string.

Useful when the GUI grows kind-based recovery actions ("Edit MAC", "Open firewall settings", etc.) — each action ID is gated on the corresponding `Failed::*` variant.
