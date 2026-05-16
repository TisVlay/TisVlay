# Replace hardcoded sleep-waits in dispatch integration tests with poll-with-deadline

The `screen_on` / `screen_off` integration tests in `crates/tisvlayd/tests/dbus_contract.rs` and `crates/tisvlayd/tests/profile_dispatch.rs` use `tokio::time::sleep(Duration::from_millis(1500))` (9 call sites at the time of writing: 7 in `dbus_contract.rs`, 2 in `profile_dispatch.rs`) to give the daemon's full state pipeline — command → mock → subscription push → state-change forwarder → D-Bus `PropertiesChanged` — time to settle before asserting on `client.list_tvs().await`. The 1500 ms budget covers the worst case (pre-flight wake + power_on probe + new-connection state push + settle + verify) with headroom; in practice most transitions land in under 100 ms.

## Why this is worth fixing

- The fixed-budget waits directly contradict the daemon's own event-driven design (Commit 6.10's `await_power_state` watch-channel pattern). Tests should match the implementation philosophy.
- Slow tests: each site burns the full 1500 ms even when the actual transition completes in ~50 ms. With 9 sites across two crates, that's ~13 s of unnecessary wall-clock that nextest's parallelism hides locally but compounds on resource-constrained CI.
- Theoretical flake surface: a loaded CI runner where state propagation drifts past 1500 ms causes the test to assert on stale state. Hasn't been observed in practice but the framework is brittle by construction.
- Hides timing bugs: a regression that pushes state-change latency from 50 ms to 1400 ms would still pass these tests.

## Path forward

Add a test helper in `tisvlay-harness` (probably alongside the existing `TestDaemon::client()` helper) that subscribes to `PropertiesChanged` on `/org/tisvlay/Tv/<uuid>` and resolves when a predicate over the cached properties is true, with a bounded deadline:

```rust
async fn await_tv_state(
    client: &DaemonClient,
    uuid: &str,
    deadline: Duration,
    predicate: impl Fn(&TvInfo) -> bool,
) -> TvInfo
```

Implementation options, easiest to hardest:

- **Poll + short sleep** (e.g., 50 ms tick) inside the deadline — simplest port, still polling but with a much smaller worst-case waste. Each test goes from `sleep(1500ms)` to `await_tv_state(client, uuid, Duration::from_secs(5), |tv| tv.power_state == "active").await`.
- **`PropertiesProxy::receive_properties_changed`** — proper D-Bus signal subscription, predicate runs on each push, exits on first match. Zero polling; matches the daemon's own event-driven shape. More plumbing in the harness, but it's a one-time cost.
- **Refactor against `await_power_state`** — couple tests directly to the daemon's internal watch channel via `TestDaemon::manager()`. Tightest signal but bypasses the D-Bus surface the tests are meant to verify.

The second option is the right target — option (a) is the cheap port if a sweep happens before option (b)'s plumbing is feasible.

## Trigger condition

File this when one of:

- A CI flake report references the dispatch tests (the 1500 ms budget proves inadequate on the runner).
- A regression in state-propagation latency ships and isn't caught by tests.
- A targeted CI-time investment pass picks up workspace tests sweepingly (the bigger refactor in option b only really pays for itself across multiple tests).

## Related

- Multi-agent code review pass surfaced this — `feat/screen-on-reliable`'s test-coverage reviewer flagged the 8 sites by line number.
- Daemon's event-driven pattern: `TvManager::await_power_state` in [crates/tisvlayd/src/tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs).
