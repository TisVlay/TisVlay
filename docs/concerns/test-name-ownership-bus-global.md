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

- Document the constraint (done — in `tisvlay-test/src/lib.rs`)
- Use nextest's `test-groups` to explicitly serialize bus-dependent
  tests within a binary if needed
- Generate unique service names per test (e.g., `org.tisvlay.Daemon.{uuid}`)
  and parameterize all proxies — high complexity, low value until the
  constraint actually bites
