# Typed pairing-failure follow-ups

**Status: the payload is typed end to end.** `PairingProgress` on a TV's `pairing/<id>/progress` key carries:
- a `PairingPhase`;
- a `FailureCode` on `failed`: the handshake's `pairing.*` reasons (from core's `PairingErrorCode`) and the first dial's `device.*` access reasons (from `DeviceErrorCode`, through the world's `DialFailure`);
- an optional `path` (the device node, for the permission case);
- the `link_mac` report on `complete`.

The power-on flow's `progress/power` key is typed the same way (`PowerProgress`, `PowerPhase`), and `Meta.automate_when` is core's `AutomateWhen`. The GUI localizes the permission case; the CLI and tray render the `message`. See [cec/protocol.md](../cec/protocol.md) for the permission case.

Remaining scope. Each item extends the vocabulary or the GUI; none needs architecture work:

- **Codes for more dial failures** (cheap per reason). `DialFailure::from_error` keeps a code only for `PermissionDenied`; every other connect error reaches clients as `pairing.connection-failed` plus a message. Typed reasons for the common ones (unreachable, refused, TLS) would let clients branch, at one `FailureCode` variant each.
- **Device errors during the handshake** (cheap). The pairing task sees only core's `PairingError`. A future device-node backend that opens its node in `start_pairing` or `build_config_from_pairing` would report a permission failure as a generic pairing failure. Mapping `core::Error::PermissionDenied` to `FailureCode::DevicePermissionDenied` at those sites keeps the code.
- **More structured params** (cheap). `path` is the template; e.g. `last_seen_at` for a future unreachable reason.
- **Kind-based recovery actions in the GUI** ("Edit MAC", "Open CEC docs", "Open firewall settings"). A real feature: each action gated on the corresponding `FailureCode`. The typed payload already reaches the wizard's outcome sink (`publish_pair_outcome`), so what's left is the per-action UI and handlers.
