## Sunshine eager-probe tests read the host

**Priority**: low
**Found by**: dependency refresh (Sept 2026), running `just check` on a workstation with Sunshine and Moonshine running

### Context

The session core's gated-wake path calls `sunshine::probe_once(EAGER_TIMEOUT)` directly (`crates/tisvlay-session/src/lib.rs`). `probe_once` scans the real `/proc` for a `sunshine` comm and, if found, sends a real `GET /serverinfo` to `127.0.0.1:47989`. Nothing in the test rig replaces either one, so `gated_wake_eagerly_refreshes_a_stale_sunshine_source` (`crates/tisvlay-session/tests/session.rs`) only passes when the host has no Sunshine process, or no listener on that port.

The observed failure: a `sunshine` process was running (passing the procfs gate) while `moonshine` held port 47989 and answered `MOONSHINE_SERVER_FREE`. `parse_serverinfo` only matches `SUNSHINE_SERVER_*`, so that answer is `Unknown`, and `Unknown` keeps the last level. The stale source never clears, the unlock's `screen_on` stays suppressed, and the test times out with `TV never reached active for Unlock`. It fails every time on such a host and passes in CI, which has neither process.

### Risk

A deterministic local failure for anyone who streams from their dev box (the feature's target audience), which trains people to ignore a red `just check`. The test also can't exercise the `Busy` / `Unknown` eager outcomes at all, since it depends on whatever the host happens to be running.

### Mitigation options

- Inject the probe: give the session core an eager-probe seam (a boxed `async Fn(Duration) -> Observation`, defaulting to `sunshine::probe_once`) and have the rig supply a scripted one. This also lets the tests cover the `Busy` and `Unknown` eager outcomes.
- Split `probe_once` into injectable procfs-scan and HTTP-query halves, so the unit tests can drive `decide` end to end without the session rig.
- Stopgap: skip the test when `sunshine_present()` is true on the host. This hides the gap rather than closing it.

### Re-evaluation triggers

- Anyone hits the local failure again, or the eager path grows another outcome worth testing.
- Moonshine support is considered: its `MOONSHINE_SERVER_*` tokens would also need `parse_serverinfo` and the procfs gate to learn a second name.
