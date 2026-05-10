## Session event tests use real-time sleeps

**Priority**: low
**Found by**: Phase C.2 session event test development

### Context

The `settle()` helper in `session_events.rs` uses
`tokio::time::sleep(Duration::from_millis(50))` to wait for the event
handler and state-change forwarder to process an injected event. This
is a real-time sleep, not deterministic paused time.

The B.0.5 clock spike proved `start_paused = true` works for
mpsc + sleep patterns. However, the session event tests involve real
D-Bus round-trips (event handler calls `daemon.list_tvs()` and
`daemon.screen_off()` over the bus), and `zbus` internally uses
timers that don't cooperate with Tokio's paused clock. Moving to
paused time would require either mocking the D-Bus calls or finding a
way to advance time that also advances zbus's internal state.

### Risk

Under heavy CI load, 50ms might not be enough for the D-Bus
round-trip to complete, causing flaky failures. The nextest `ci`
profile retries once, which masks occasional flakes but doesn't fix
them.

### Mitigation options

- Increase `settle()` to 100–200ms (buys headroom, costs test speed)
- **Poll for the expected state with a timeout** instead of a fixed
  sleep (recommended). Polls at short intervals, succeeds as soon as
  the condition is met, retries up to a timeout under CI load:

  ```rust
  async fn wait_for(f: impl Fn() -> bool, timeout: Duration) {
      let deadline = tokio::time::Instant::now() + timeout;
      while !f() && tokio::time::Instant::now() < deadline {
          tokio::time::sleep(Duration::from_millis(5)).await;
      }
      assert!(f(), "condition not met within {:?}", timeout);
  }
  ```

  This makes tests both faster (succeeds in ~5ms instead of sleeping
  a fixed 50ms) and more resilient (retries up to the deadline under
  CI load instead of failing on first miss).

- Factor out D-Bus from the event handler so pure-logic tests can use
  paused time, and reserve D-Bus round-trip tests for a slower
  integration tier
