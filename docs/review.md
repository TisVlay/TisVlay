# Review: Phases A, B, C — Discovery/pairing, daemon integration tests, session tests (main..HEAD)

Review of 30 commits implementing Phases A (MockHandle for discovery/pairing), B (daemon
integration tests with private D-Bus), and C (session service lib extraction + tests).
Also includes a bug fix for TV ID / D-Bus path inconsistency discovered through testing.

Build: clean. All tests pass (133+ across 12 test suites).

Coverage: 20% → 36% lines (`tv_manager.rs` from 0% → 30%).

---

## AA1. MockHandle — unified test control surface

**Files:** `crates/tisvlay-mock/src/handle.rs`, `state.rs`, `pairing.rs`, `discovery.rs`

`MockHandle` exposes a clean symmetric API for discovery and pairing:
- `publish_tv(DiscoveredTv)` / `clear_discovered()` — populate the "network"
- `simulate_user_accept(tv_id)` / `simulate_user_reject(tv_id)` / `simulate_pairing_timeout(tv_id)` — drive pairing outcomes

Each test gets its own `(BackendRegistry, MockHandle)` pair via `create_test_registry()`.
No shared state between tests — parallel by construction.

The `PairingShorthand` config (`pairing = "accept"|"reject"|"timeout"`) auto-schedules
the corresponding `simulate_user_*` call, so simple tests don't touch the handle.
If both shorthand and handle are set, the handle wins — correct precedence.

`MockState` uses `std::sync::Mutex` (not tokio) — brief critical sections, never held
across an `.await`. The connector's `start_pairing` spawns a `tokio::task` that polls
the state machine, matching the real LG backend's async pairing architecture.

**Severity:** Info (well-designed, matches plan's design decisions)

---

## AA2. `create_test_registry` vs `create_registry` — clean separation

**File:** `crates/tisvlay-backends/src/lib.rs`

Production `create_registry()` calls `new_production_parts()` — the handle is dropped
immediately, so production code can never drive test hooks. `create_test_registry()`
calls `new_test_parts()` and surfaces the handle.

`BackendRegistry::discover_tvs(vendor, timeout)` is ungated (no `#[cfg(feature = "discovery")]`)
so mock discovery works in test builds that don't enable the full SSDP discovery feature.

**Severity:** Info (correct feature design)

---

## AA3. 11 mock discovery/pairing integration tests — thorough coverage

**File:** `crates/tisvlay-backends/tests/mock_discovery.rs`

Covers: empty network, ordered publish, dynamic publish-after-discover, clear,
happy-path pairing, reject, timeout, shorthand variants, handle-overrides-shorthand,
and mixed-mode multi-TV. Each test uses its own `create_test_registry()` call.

**Severity:** Info (comprehensive coverage of the mock API)

---

## AA4. PrivateBus — robust private dbus-daemon spawner

**File:** `crates/tisvlay-test-support/src/dbus.rs`

- `LazyLock<PrivateBus>` — one daemon per test binary, shared via `PrivateBus::shared()`
- `PR_SET_PDEATHSIG = SIGKILL` in `pre_exec` — kernel kills the daemon on test crash
- Env var scrubbing (`DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`,
  `DBUS_STARTER_*`) before spawn — isolates from the user session
- `connect()` uses `zbus::connection::Builder::address(...)` — bypasses the per-process
  `OnceCell` cache that would alias all tests to one connection

This is the correct architecture per the plan's design decision §3 and §4. The zbus
connection cache issue was the key insight from the research.

**Severity:** Info (correct implementation of a subtle requirement)

---

## AA5. TestDaemon — fully wired daemon on private bus

**File:** `crates/tisvlayd/src/test_harness.rs`

`TestDaemon::with_one_mock_tv(tv_id)` builds a complete daemon: `SystemConfig` in a
tempdir, `TvManager`, `BackendRegistry` with mock, full D-Bus surface (ObjectManager,
Manager1, per-TV Tv1.* interfaces), state-change forwarder — all on the shared
`PrivateBus`. Returns the connection, manager, and `MockHandle`.

`TestDaemon::with_config(SystemConfig)` for custom setups. The `mock_tv_config()`
helper builds a standard single-TV config.

The forwarder task is aborted on drop — correct cleanup. The connection is released
on drop, unregistering the well-known name.

**Severity:** Info (clean harness design)

---

## AA6. tisvlayd lib+bin split and BusKind extraction

**Commit:** 59ebc95

`tisvlayd/src/main.rs` drops from ~440 to ~145 lines. Public modules: `tv_manager`,
`dbus`, `state_forwarder`, `logind`, `netlink`, `network_watcher`, `test_harness`.

`BusKind` enum (`Session | System | Address(String)`) moved to `tisvlay-core` (dbus
feature-gated) with `builder()` method. Both `tisvlayd` and `tisvlay-session` use it.
No code path divergence between production and test bus setup.

`state_change_forwarder` moved to `src/state_forwarder.rs` so it stays in the lib
with access to the private cached fields on `Tv*Service` structs.

**Severity:** Info (correct lib+bin pattern, shared BusKind)

---

## AA7. Profile dispatch tests — tv_manager.rs 0% → 30%

**File:** `crates/tisvlayd/tests/profile_dispatch.rs`

5 tests exercise `execute_system_event` through the full pipeline:
- `shutdown_disconnects_connected_tv`
- `sleep_disconnects_connected_tv`
- `reboot_does_not_disconnect_tv` (default profile, action=None)
- `no_session_does_not_disconnect_tv`
- `reconnect_sets_connected_property_on_dbus`

Each builds a `TestDaemon`, connects the mock TV, fires a system event, and asserts
the outcome. The D-Bus Connected property is verified via `DaemonClient::list_tvs()`.

**Severity:** Info (significant coverage gain)

---

## AA8. Automated gate composition tests

**File:** `crates/tisvlayd/tests/automated_gate.rs`

5 gate tests covering the 4-input composition (managed × automate_when × primary_input
× current_input):
- `unmanaged_tv_is_not_automated`
- `on_primary_fires_when_input_matches`
- `on_primary_skips_when_input_differs`
- `off_primary_fires_when_input_differs`
- `off_primary_skips_when_input_matches`

2 multi-TV concurrency tests:
- `shutdown_only_affects_managed_tvs`
- `mixed_profiles_dispatch_independently`

**Severity:** Info (comprehensive gate coverage)

---

## AA9. D-Bus contract tests

**File:** `crates/tisvlayd/tests/dbus_contract.rs`

6 tests verify the full zbus pipeline (client → D-Bus → TvManager → MockConnection →
subscription push → forwarder → property cache → client property read):
- `connected_tv_reports_initial_mock_state`
- `set_volume_round_trips_through_dbus`
- `set_mute_round_trips_through_dbus`
- `screen_off_updates_power_state_and_screen_flag`
- `set_backlight_round_trips_through_dbus`
- `get_supported_vendors_includes_mock`

**Severity:** Info (validates the end-to-end property pipeline)

---

## AA10. Pair round-trip tests → discovered TV ID bug → fix

**File:** `crates/tisvlayd/tests/pair_round_trip.rs`

4 tests exercise the full pairing flow through D-Bus:
- `pair_happy_path`: pair, verify TV appears in list
- `pair_then_control`: pair, screen_off, verify interface works
- `pair_rejected`: simulate_user_reject, verify error, TV not in list
- `pair_unmanaged`: pair with managed=false, verify flag persists

These tests **discovered** the TV ID / D-Bus path inconsistency (docs/bugs.md):
hyphens in TV IDs diverged from underscore-sanitized D-Bus path suffixes. The fix
(795a611) restricts TV IDs to `[A-Za-z0-9_]`, validates at config load, fixes
auto-generation, and changes `AddTv` to return the canonical ID.

**Severity:** Info (tests found a real bug and it was fixed)

---

## AA11. Session service lib extraction + TestSession harness

**Commit:** cf7a0eb

`tisvlay-session` split into lib+bin (same pattern as tisvlayd). `SessionState`,
`event_handler`, action dispatch moved to `lib.rs`. `main.rs` becomes thin entry point.

`register_session_dbus_on(bus, ...)` accepts `BusKind` for test bus injection.
`SessionMonitor::start_on(conn)` for the same reason.

`FakeScreenSaver` in test-support: hand-rolled `org.freedesktop.ScreenSaver` stub
with `ActiveChanged` signal emission, matching the `FakeLogind` pattern.

`TestSession` harness: wires TestDaemon + DaemonClient + SessionState +
FakeScreenSaver + event handler on the shared private bus.

**Severity:** Info (clean extraction, enables session testing)

---

## AA12. Session integration tests

**Files:** `crates/tisvlay-session/tests/session_events.rs`, `session_dbus_contract.rs`

6 session event tests: lock→screen_off, unlock→screen_on, idle→screen_off,
idle_resume→screen_on, greeter_wake→screen_on, duplicate lock idempotent.

5 session D-Bus contract tests: ListUserProfiles includes "default",
GetTvUserProfile returns "default", GetEffectiveUserProfile valid JSON,
SetTvUserProfile emits UserProfileChanged, SetTvUserProfile rejects unknown.

**Severity:** Info (session service now has first-class test coverage)

---

## AA13. tracing-test + tracing-mock integration

**Commits:** c706825, 1a7b27a

`tracing-test` with `no-env-filter` feature ensures events from dependency crates
(tisvlay-mock, tisvlay-test-support) are captured. `#[traced_test]` auto-dumps logs
on failure.

`tracing-mock` regression tests assert specific `#[instrument]` span names and event
fields: `publish_tv` carries `tv_id`, `simulate_user_accept` carries `tv_id` and
`outcome`. If someone removes an `#[instrument]` or renames a span, the test fails.

**Severity:** Info (observability regression protection)

---

## AA14. StateWatcher tests on private bus

**File:** `crates/tisvlay-core/tests/state_watcher.rs`

2 tests verify the name-watch lifecycle:
- `watcher_emits_daemon_appeared_when_name_claimed`
- `watcher_emits_daemon_disappeared_when_name_released`

Both use `PrivateBus::shared()` and exercise `StateWatcher::from_connections` against
a real dbus-daemon. The tray and future GUI depend on this for instant daemon availability
tracking.

**Severity:** Info (validates the StateWatcher on a real bus)

---

## AA15. TV ID restriction — correct fix for a testing-discovered bug

**Commit:** 795a611

The fix restricts TV IDs to `[A-Za-z0-9_]` and validates at config load with a clear
error message and suggested fix. Auto-generated IDs (from pairing) use `_` instead of
`-`. `AddTv` returns `(ObjectPath, String)` so the client gets the canonical ID directly.

5 new unit tests for validation and ID generation. All test fixtures and doc examples
migrated from hyphens to underscores.

This is a good example of testing finding real bugs: the pair_round_trip test failed
because `DaemonClient::pair_tv` extracted the TV ID from the D-Bus path (lossy) instead
of receiving it directly from the daemon.

**Severity:** Info (bug found by tests, fix is clean)

---

## AA16. Clock spike validates deterministic time for future tests

**File:** `crates/tisvlay-test-support/tests/clock_spike.rs`

Validates `tokio::time::pause()` + `advance()` with the exact pattern TvManager uses
(mpsc receiver + sleep loop). Documents caveats: `start_paused = true` already freezes
time (explicit `pause()` panics), and `tokio::time::Instant` reads virtual time (wall
clock guard must use `std::time::Instant`).

**Severity:** Info (validates infrastructure for future timeout tests)

---

## AA17. `DaemonClient::from_connection` — correct constructor design

**Commit:** 70c5fbd

General-purpose constructor, not testing-specific. No feature flag, no `doc(hidden)`.
Bypasses zbus's `OnceCell` cache. Same pattern for `StateWatcher::from_connections`.

**Severity:** Info (correct per plan design decision §6)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| AA1 | **Info** | MockHandle is well-designed unified test control surface |
| AA2 | **Info** | Production/test registry separation is clean |
| AA3 | **Info** | 11 discovery/pairing tests are thorough |
| AA4 | **Info** | PrivateBus handles zbus cache, pdeathsig, env scrubbing |
| AA5 | **Info** | TestDaemon is fully wired with correct drop semantics |
| AA6 | **Info** | lib+bin split and shared BusKind are clean |
| AA7 | **Info** | Profile dispatch tests significant coverage gain |
| AA8 | **Info** | Automated gate composition fully covered |
| AA9 | **Info** | D-Bus contract validates end-to-end property pipeline |
| AA10 | **Info** | Pair round-trip tests found a real bug |
| AA11 | **Info** | Session service lib extraction enables testing |
| AA12 | **Info** | Session event + D-Bus contract tests comprehensive |
| AA13 | **Info** | tracing-mock regression tests protect observability |
| AA14 | **Info** | StateWatcher tests validate name-watch lifecycle |
| AA15 | **Info** | TV ID restriction correctly fixes testing-discovered bug |
| AA16 | **Info** | Clock spike validates deterministic time infrastructure |
| AA17 | **Info** | from_connection constructors are general-purpose, no feature flag |

## Overall Assessment

This is the largest testing infrastructure investment in the project — ~4700 lines net,
going from 20% to 36% coverage. The architecture is well-designed: private dbus-daemon
per test binary avoids the zbus `OnceCell` cache issue, `MockHandle` provides a clean
symmetric API, and the `TestDaemon`/`TestSession` harnesses wire the full D-Bus surface
onto the private bus.

The most valuable outcome is that **testing found a real bug** (AA10/AA15): TV IDs with
hyphens diverged from their D-Bus path suffixes, causing `pair_tv()` to return wrong IDs.
The fix restricts the ID alphabet and returns the canonical ID from `AddTv`. This is
exactly the kind of regression that would have been reported by users had it shipped.

No actionable items. All 17 findings are info-level.

## Idiomatic Rust observations

- **`MockState` uses `std::sync::Mutex`** (not `tokio::sync::Mutex`) with brief critical
  sections. This is correct — `tokio::sync::Mutex` is only needed when the lock is held
  across `.await` points, which `MockState` never does.

- **`PrivateBus::shared()` uses `LazyLock`** (stabilized in Rust 1.80) instead of
  `once_cell::sync::Lazy`. Good — one fewer dependency.

- **`pre_exec` for `PR_SET_PDEATHSIG`** uses `unsafe` correctly — the only operation
  in the hook is the `libc::prctl` call, which is async-signal-safe.

- **`TestDaemon` aborts its forwarder on `Drop`** via `self.forwarder.abort()`. This
  is fine — the forwarder is a pure event loop with no cleanup requirements. The
  connection drop releases the D-Bus name.

- **`PairingOutcome` enum** (`Accept`, `Reject`, `Timeout`) is a clean state machine
  input. The `PairingShorthand` config maps to it, and the handle methods set it
  directly. No ambiguity about precedence (handle wins over shorthand).
