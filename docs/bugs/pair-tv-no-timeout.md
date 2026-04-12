## `TvManager::pair_tv` has no timeout on `wait_for_completion`

**Status**: open
**Severity**: medium — can hang a D-Bus call indefinitely
**Found by**: mock_discovery test design (the `drain_pairing` helper
wraps in `tokio::time::timeout` to avoid test hangs)

### Problem

`tisvlayd/src/tv_manager.rs:1216` calls
`session.wait_for_completion().await` with no timeout. If the TV
never responds to the pairing prompt (e.g., TV is powered off mid-pair,
network drops, firmware bug), the D-Bus `AddTv` call blocks forever.
The daemon remains responsive for other TVs but the calling CLI
process hangs.

The core `PairingSession` deliberately omits a built-in timeout
(timeouts belong to the caller), which is the right design. But the
caller (`TvManager`) doesn't apply one.

### Fix plan

Wrap `wait_for_completion()` in `tokio::time::timeout()` with a
configurable deadline (default: 60s for network backends, could be
longer for Bluetooth). Return `PairingError::Timeout` on expiry.

The timeout must also cancel the `PairingSession` so the TV stops
showing the pairing prompt. Without cancel, the TV keeps showing
"Accept?" until its own internal timeout and the user has to dismiss
it manually:

```rust
match tokio::time::timeout(deadline, session.wait_for_completion()).await {
    Ok(result) => result,
    Err(_) => {
        let _ = session.cancel(); // dismiss the TV prompt
        Err(PairingError::timeout())
    }
}
```

### Affected files

- `crates/tisvlayd/src/tv_manager.rs` — `pair_tv()` needs timeout + cancel
