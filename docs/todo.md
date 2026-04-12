# Design Concerns & Technical Debt

Items that aren't bugs but represent fragility, suboptimal patterns,
or deferred decisions worth revisiting.

---

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
- Poll for the expected state with a timeout instead of a fixed sleep
- Factor out D-Bus from the event handler so pure-logic tests can use
  paused time, and reserve D-Bus round-trip tests for a slower
  integration tier

---

## `TestDaemon` / `TestSession` name ownership is bus-global

**Priority**: low  
**Found by**: Phase B/C harness design

### Context

Both `TestDaemon` and `TestSession` register well-known D-Bus names
(`org.tisvlay.Daemon`, `org.tisvlay.Session`) on the shared private
bus. D-Bus name ownership is bus-global — only one owner at a time.

Nextest runs each test binary in its own process (and therefore its
own private bus), so tests across binaries never collide. But tests
within a single binary that both create a `TestDaemon` will get a
"name already taken" error if they run in parallel.

Currently this works because nextest runs each `#[tokio::test]` in
its own process. If we ever switch to a runner that shares processes
(e.g., `cargo test` without nextest), tests would need serialization.

### Mitigation options

- Document the constraint (done — in `tisvlay-test-support/src/lib.rs`)
- Use nextest's `test-groups` to explicitly serialize bus-dependent
  tests within a binary if needed
- Generate unique service names per test (e.g., `org.tisvlay.Daemon.{uuid}`)
  and parameterize all proxies — high complexity, low value until the
  constraint actually bites
