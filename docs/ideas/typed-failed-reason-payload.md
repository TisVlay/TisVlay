# Typed `Failed { reason }` payload across daemon error vocabulary

**Status: core landed; the architecture is in place.** The idea's whole
premise — replacing the bare `(s)` reason string with a typed,
variant-tagged wire payload that clients branch on by kind — shipped for
the pairing flow. `Manager1.PairingProgress` now carries a typed
`PairingProgressDetail` `a{sv}` (the variant-tagged dict this idea called
for); on the `failed` phase it holds a namespaced reason `code`
(`device.permission-denied`, `pairing.rejected`, …) and an optional `path`
param. The GUI localizes/branches on the `code`; CLI/tray render the
`message`. `PairingErrorCode` was retrofitted to namespaced `pairing.<kebab>`
codes (matching `errors/power.rs` / `config.rs`), and a `device` error
domain (`errors/device.rs`, `DeviceErrorCode`) was added for the
transport-agnostic `Error::PermissionDenied`. The permission case is the
first consumer. See [design.md](../design.md) (§ Signals — `PairingProgress`)
and [cec/protocol.md](../cec/protocol.md).

Remaining scope — **no architecture work left**; each item just extends the
established projection (`tv_manager::failure_reason`) or vocabulary. The
first three are cheap; the last is the one genuinely feature-sized piece:

- **Retype `TvManagerError::PairingFailed(Box<dyn Error>)` to carry a typed `PairingError`** (cheap). Today `failure_reason` recovers the code via `downcast_ref::<PairingError>()`, which only works because the *permission* case travels as `ConnectionFailed { source }` (a plain match). If a future device-node backend opens its node *during* `start_pairing`/`build_config_from_pairing`, its `Error::PermissionDenied` boxes into `PairingFailed` and the downcast misses, degrading the code to `pairing.unknown`. Making the variant carry a typed error (mapping `core::Error` → a coded `PairingError` at the boxing sites) drops the downcast and lets every code survive.
- **Broaden the vocabulary** to every terminal `TvManagerError` beyond what pairing already projects (cheap — add arms to `failure_reason`; handshake-native codes `pairing.rejected`/`timeout`/… already flow).
- **More structured params** in the detail dict for other reason kinds (cheap — the `path` key is the template; e.g. `last_seen_at` for a future `TvUnreachable`).
- **Kind-based recovery actions in the GUI** ("Edit MAC", "Open CEC docs", "Open firewall settings") — a real feature: each action gated on the corresponding reason `code`. The typed payload already reaches the wizard's outcome sink (`publish_pair_outcome`), so the plumbing is done; what's left is the per-action UI + handlers.
