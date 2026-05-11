## `run_pairing_handshake` has no timeout on `wait_for_completion`

**Status**: open
**Severity**: low — async-pair refactor downgraded this from
"client hangs forever" to "client sees no terminal phase" (clients
can apply their own drain-timeout on the signal stream)
**Found by**: mock_discovery test design (the `drain_pairing` helper
wraps in `tokio::time::timeout` to avoid test hangs)

### Problem

`tisvlayd/src/tv_manager.rs::run_pairing_handshake` (around line
1925) calls `session.wait_for_completion().await` with no timeout.
If the TV never responds to the pairing prompt (powered off mid-pair,
network drops, firmware bug), the spawned `run_pair_task` /
`run_re_pair_task` hangs indefinitely. No terminal `PairingProgress`
fires, the `pairing_sessions` map entry persists (eventually freed
when the task is aborted on daemon shutdown), and the corresponding
slot stays claimed (`pairing_in_flight = true` for re-pair).

The core `PairingSession` deliberately omits a built-in timeout
(timeouts belong to the caller), which is the right design. But the
caller (`run_pairing_handshake`) doesn't apply one.

### Pre-refactor severity (historical)

Before the async-pair refactor, `Manager1.AddTv` blocked the D-Bus
RPC until the handshake completed. A hung pairing meant the calling
CLI process hung along with it (the D-Bus call never returned). The
async-spawn shape decoupled this: AddTv returns the tv_id
immediately, and the hang now happens in a daemon-side task that
clients can ignore.

### Current client-side mitigations

`DaemonClient::drain_pair_to_terminal` doesn't timeout internally,
but callers can wrap it in `tokio::time::timeout`:

```rust
let outcome = tokio::time::timeout(
    Duration::from_secs(120),
    DaemonClient::drain_pair_to_terminal(stream, &tv_id, on_progress),
).await;
match outcome {
    Ok(Ok(terminal)) => /* handle */,
    Ok(Err(e)) => /* stream ended early */,
    Err(_) => {
        // Best effort: tell the daemon we're giving up so it
        // releases the slot + dismisses the TV prompt.
        let _ = client.cancel_pair(&tv_id).await;
        // Surface the timeout to the user.
    }
}
```

The GUI wizard's existing Cancel button already routes to
`Manager1.CancelPair(tv_id)` so manual timeout (user clicks Cancel)
works today.

### Fix plan

Wrap `wait_for_completion()` inside `run_pairing_handshake` in
`tokio::time::timeout()` with a configurable deadline (default: 60s
for network backends; could be longer for Bluetooth). On timeout:

1. Send `PairingCommand::Cancel` to the session so the backend
   dismisses the TV prompt.
2. Return `Err(PairingError::Timeout)` so the outer task emits a
   terminal `failed` PairingProgress with a meaningful reason.
3. Clear the `pairing_in_flight` slot (already handled by the outer
   task's cleanup).

```rust
let result = match tokio::time::timeout(deadline, session.wait_for_completion()).await {
    Ok(result) => result,
    Err(_) => {
        // Dismiss the TV's pairing prompt before bailing.
        let _ = session.command_sender().send(PairingCommand::Cancel).await;
        Err(PairingError::timeout())
    }
};
```

### Affected files

- `crates/tisvlayd/src/tv_manager.rs::run_pairing_handshake` —
  add timeout + cancel-on-timeout
- `crates/tisvlay-core/src/config.rs` — surface a configurable
  `pairing_timeout_seconds` (per-vendor optional)
