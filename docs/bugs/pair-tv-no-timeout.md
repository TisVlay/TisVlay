## A pairing session has no daemon-side deadline

**Status**: open
**Severity**: low — clients can cancel; only a vendor without its own bound can wait forever
**Found by**: pairing test design (the test helpers wrap the progress drain in a timeout to avoid hangs)

### Problem

`pairing_task` in `crates/tisvlay-world/src/backend.rs` forwards a `PairingSession`'s states to the world until one is terminal, waiting on `rx.changed()` with no deadline. If the device never answers (powered off mid-pair, the network drops, a firmware bug), the session never reaches a terminal state: no `failed` phase is published on `pairing/<uuid>/progress`, and the session holds its slot until it's cancelled or the daemon stops.

LG bounds itself: its handshake gives the TV's prompt `PAIRING_MS` (60 s) before failing. A vendor without such a bound inherits the open-ended wait. The mock's `pairing = "manual"` mode is one: it waits for a test handle that production doesn't have ([concerns/cli-pair-vendor-fields.md](../concerns/cli-pair-vendor-fields.md)).

`PairingSession` deliberately has no built-in timeout, since how long a prompt deserves is the caller's call. The caller here doesn't apply one.

### Mitigations today

- `manager/rpc/cancel-pair` (`DaemonClient::cancel_pair`) ends a session; the GUI wizard's **Cancel** uses it.
- A client draining progress can wrap the drain in `tokio::time::timeout` and cancel on expiry, as the tests do.

### Fix plan

Bound the wait in `pairing_task` with a deadline: a daemon default, which a backend could lengthen for slow transports. On expiry, send `PairingCommand::Cancel` so the backend dismisses the device's prompt, then publish a terminal `failed` state with a timeout code, so every client sees the same ending.
