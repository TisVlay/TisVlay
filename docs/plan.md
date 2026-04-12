# Plan: Testing & Measurement Framework

## Goal

Establish an extensible testing and measurement framework for the tisvlay workspace. The framework should:

1. Make it cheap to add new tests (low friction → more tests get written)
2. Catch regressions before they hit production
3. Measure code coverage and treat it as a property to monitor over time
4. Support LLM-assisted development (clear conventions, fast feedback, structured outputs)
5. Run in CI on every PR with results visible inline
6. Lay the groundwork for a mock backend that enables testing high-level features (pairing, discovery, profiles, CLI, GUI) without real hardware

## Status

**Phases 1–6 complete.** The framework is in place:
- Ratchet coverage at 20.24% lines (baseline 19%)
- 94 tests pass in <1s via nextest; JUnit XML published via dorny/test-reporter
- insta snapshots pin the `TvInfo` wire format
- rstest converts three flat test functions into 7 labelled cases
- proptest runs ~1,800 randomised inputs across 7 properties
- tisvlay-mock crate implements the full `TvBackend` / `TvConnector` / `TvConnection` surface with push-state subscriptions

**Phase 7 deferred** until the repo becomes public (the only remaining work is uncommenting the Codecov upload block). Phases **A** and **B** below are the natural follow-ups now that the mock backend exists.

## Design principles

These are the choices that shape the framework. Each is justified inline.

### 1. Coverage as a property to monitor, not a gate

Set up `cargo-llvm-cov` to generate reports locally and in CI. Upload to Codecov (free for OSS) for trend tracking and PR diff annotations. **Do not** set a hard minimum threshold initially — the current coverage is unknown and an arbitrary minimum (e.g., 80%) would either be trivially met or block all PRs. Instead:

1. Measure baseline coverage
2. Add a "coverage must not decrease" CI check (`--fail-under-lines` set to current baseline, bumped on PRs that improve coverage)
3. Track trend over time via Codecov

This is the "ratchet" approach from the primer. It pressures new code to come with tests without blocking the existing untested surface.

### 2. Layered test types matching their costs

| Type | Where | When | Tool |
|------|-------|------|------|
| Unit tests | `#[cfg(test)] mod tests` inside source | every change | built-in |
| Integration tests | `crates/*/tests/` | every change | built-in |
| Doc tests | `///` examples in `pub` items | every change | built-in |
| Parameterized tests | unit/integration tests | every change | `rstest` |
| Snapshot tests | `crates/*/tests/snapshots/` | every change | `insta` |
| Property tests | inside unit/integration tests | every change | `proptest` |
| Mock backend tests | `crates/tisvlay-mock/` + `tisvlay-backends/tests/` | every change | mock crate |
| Fuzzing | `fuzz/` (future) | scheduled | `cargo-fuzz` |

### 3. Single test runner: `cargo-nextest`

Process-per-test isolation matters because the daemon code uses global state (D-Bus, netlink, file paths). JUnit XML output for inline PR test results. Faster than `cargo test` on cold runs. Doc tests still run through `cargo test --doc` because nextest doesn't support them.

### 4. Keep it transport-agnostic and vendor-agnostic

The same architecture lessons from the daemon apply: test layers should not know about specific vendors. The mock backend implements `TvBackend`/`TvConnector`/`TvConnection` so any test that exercises the daemon can swap LG for the mock without changes.

### 5. LLM-friendly conventions

- Predictable test layout: unit tests in `mod tests`, integration tests in `tests/`, snapshot files in `tests/snapshots/`. No magic.
- Snapshot tests for anything with structured output (config parsing, D-Bus message format, CLI output)
- Fast feedback: full test suite in under 30s; coverage report in under 60s
- Test names describe behavior, not method names: `nm_disconnect_does_not_affect_other_nics` not `test_disconnect`
- `cargo nextest --profile ci` outputs JUnit XML which LLMs can parse from CI logs to identify exactly which test broke

## Implementation plan

### Phase 1 — Coverage measurement ✅ complete

Set up `cargo-llvm-cov` with ratchet check. Baseline measured at 19.59% → currently 20.24% lines. `just cov`, `just cov-html`, `just cov-lcov`, `just cov-ratchet` recipes. Codecov upload block pre-written but commented out until the repo is public.

### Phase 2 — Test runner upgrade (`cargo-nextest`) ✅ complete

Installed via flake + `taiki-e/install-action`. `.config/nextest.toml` with `default` and `ci` profiles. CI uses `just test-ci` which runs nextest under coverage instrumentation and publishes JUnit XML via `dorny/test-reporter@v3` with `list-suites: failed` / `list-tests: failed`. Doc tests run separately via `cargo test --doc`.

### Phase 3 — Snapshot tests (`insta`) ✅ complete

`insta` in workspace dev-deps, `cargo-insta` in flake. Three snapshot tests in `crates/tisvlay-core/tests/tv_info_snapshot.rs` pin the `TvInfo` wire format (connected / disconnected / list shapes). `just test-snapshot-review` and `just test-snapshot-accept` for the review workflow.

### Phase 4 — Parameterized tests (`rstest`) ✅ complete

`rstest` in workspace dev-deps (la10736/rstest 0.26). Three flat `tv_path` tests consolidated into one `#[rstest]` with 7 labelled cases, adding coverage for four edge cases that weren't worth writing separately before.

### Phase 5 — Property tests (`proptest`) ✅ complete

`proptest` in workspace dev-deps. Three properties on `tv_path` (suffix validity, char count preservation, valid-input passthrough) and four on `MacAddress` (parse/display roundtrip, separator equivalence, case insensitivity, length rejection). Each proptest runs 256 random inputs per invocation, ~1,800 distinct cases total.

### Phase 6 — Mock backend skeleton ✅ complete

`crates/tisvlay-mock/` implements the full trait surface with an in-memory state machine:
- `MockBackend` — vendor metadata (`Capabilities::ALL`)
- `MockConnector` — `Transport::Network`, `survives_source_ip_change() -> false` (matches LG), stub pairing that completes instantly
- `MockConnection` — `Arc<Mutex<TvState>>` backing, real `subscribe_state()` pushing cumulative snapshots on every mutation (matches LG's SSAP model)
- `MockVendorConfig` — configurable initial state

Gated behind the `mock` Cargo feature on `tisvlay-backends` (default-off in release, default-on in workspace `--all-features` dev/test builds). Distro package build (`cargo build -p tisvlayd`) links zero mock code. Six integration tests at `crates/tisvlay-backends/tests/mock_backend.rs` cover registration, transport semantics, state mutation, push subscription pipeline, input validation, and power_off lifecycle.

### Phase 7 — CI integration polish (deferred)

**Deferred until the repo becomes public.** The only remaining work is uncommenting the Codecov upload block in `.github/workflows/ci.yml` (one-line change). Test workflow already publishes JUnit XML; coverage workflow already enforces the ratchet; permissions and action versions are up to date.

### Phase A — Discovery + pairing user story (next)

**Goal**: extend the mock backend to support device discovery and failure-mode pairing so the entire "new user pairs their first TV" flow can be tested end-to-end.

**Scope**:

1. **`MockDiscovery`** implementing `TvDiscovery`. Reads the list of discoverable TVs from a test fixture so different tests can configure different networks.
   - Fixture format TBD (env var? TOML file? `MockDiscoveryConfig` struct?) — see open questions below
2. **Register in `create_registry`** under the `mock` feature, alongside the connector
3. **`MockPairingMode`** enum on `MockConnector` allowing tests to exercise:
   - `Accept` — current behavior (completes instantly with dummy credentials)
   - `Reject` — returns `PairingError::rejected()` to simulate the user pressing "no" on the TV remote
   - `Timeout` — returns `PairingError::timeout()` after a configurable delay
   - `DelayedAccept(Duration)` — useful for testing UX behavior during a slow accept
4. **Integration tests** covering the user stories:
   - Discover → pair → save config → connect (the happy path)
   - Discover → pair → user rejects → error surfaces correctly
   - Discover → pair → timeout → error surfaces correctly
   - Discover empty network → clear "no TVs found" behavior
   - Multiple TVs discovered → pick one
5. **CLI snapshot test** for `tisvlay pair` output via `insta-cmd`

**What this unlocks**:
- The pairing wizard becomes testable without hardware
- Future GUI pairing dialog has a regression suite
- Error-path CLI output is snapshotted

**Files**:
- `crates/tisvlay-mock/src/discovery.rs` (new)
- `crates/tisvlay-mock/src/connector.rs` — add pairing mode state
- `crates/tisvlay-backends/src/lib.rs` — register `MockDiscovery`
- `crates/tisvlay-backends/tests/mock_discovery.rs` (new) — integration tests
- `crates/tisvlay-cli/tests/pair_snapshot.rs` (new) — CLI snapshot test
- `.claude/CLAUDE.md` — document the pairing failure modes

**Estimated effort**: ~1-2 focused sessions.

### Phase B — Daemon integration tests (larger)

**Goal**: test the daemon's core flows (profile dispatch, multi-TV orchestration, D-Bus contract) against mock TVs, in-process, without needing a real D-Bus session.

**Scope**:

1. **Test harness helper** in `crates/tisvlayd/tests/common/`:
   - Loads a test config from a fixture directory
   - Boots `TvManager` in-process with mock TVs
   - Optionally registers the D-Bus interfaces on a temporary session bus (using `zbus::connection::Builder::session()`)
   - Exposes the `TvManager` and a scoped D-Bus connection to tests
   - Tears down cleanly

2. **TvManager unit tests** exercising the currently-0%-covered dispatch code in `tv_manager.rs`:
   - Profile resolution (`on_startup`, `on_shutdown`, `on_sleep`, `on_wake`, `on_session_lock`, `on_session_unlock`, `on_idle_timeout`, `on_idle_resume`, `on_no_session`) against mock TVs
   - `Automated` gate composition (managed × automate_when × primary_input × current_input)
   - Multi-TV concurrent operations (two mocks with different profiles respond independently)
   - `SetTvOption` for `managed`, `name`, `profile.system`, `primary_input`
   - Config reload live-updating property cache

3. **D-Bus contract tests** (most ambitious):
   - Spawn the daemon's D-Bus interfaces on a temp session bus
   - Connect a `DaemonClient` from `tisvlay-core` to it
   - Drive mock TVs via the client, assert state changes propagate through the real D-Bus layer
   - This is the test shape that currently exists only in `tisvlay-core/tests/dbus_client.rs` (for name-watch behavior) — we'd extend it to cover the full Tv1.* interface surface
   - Would catch regressions in the `InterfaceRef::get_mut()` property cache and `PropertiesChanged` signal emission

4. **Session service integration tests**:
   - Spin up the session service against a daemon backed by mocks
   - Inject logind `PrepareForSleep(true)` signals
   - Assert the right `SessionAction` fires on the right TVs
   - Covers the user profile resolution and event replay paths

5. **StateWatcher integration** (tests for the watcher we extracted in the earlier branch):
   - Test that the tray's `StateWatcher` correctly reflects daemon state changes
   - Mock daemon emits PropertiesChanged → watcher debounces → emits DaemonStateChanged event

**What this unlocks**:
- Profile dispatch code goes from 0% coverage to well-covered
- The D-Bus contract becomes a first-class tested interface
- Regressions in the session service event flow surface in CI, not in user reports
- Refactors of `TvManager` become safe

**Files**:
- `crates/tisvlayd/tests/common/mod.rs` (new) — harness
- `crates/tisvlayd/tests/profile_dispatch.rs` (new)
- `crates/tisvlayd/tests/automated_gate.rs` (new)
- `crates/tisvlayd/tests/dbus_contract.rs` (new)
- `crates/tisvlayd/tests/concurrent_tvs.rs` (new)
- `crates/tisvlay-session/tests/session_events.rs` (new)
- `crates/tisvlay-tray/tests/state_watcher.rs` (new)

**Estimated effort**: 1-2 weeks of focused work. This is where the coverage numbers jump significantly because `tv_manager.rs`, `logind.rs`, `session_dbus.rs`, and `session_monitor.rs` are currently all at 0%.

**Prerequisites**:
- Phase A complete (so the mock supports the full range of scenarios these tests need)
- Some refactoring of `TvManager` to expose test hooks (e.g., injectable clocks for timeout testing) — scope TBD as we go

## Out of scope (for now)

These belong in a future plan once the framework is established:

- **Mutation testing** (`cargo-mutants`) — worth running once Phase B lands and coverage jumps; too noisy until then
- **Fuzzing** (`cargo-fuzz`) — needs clear input-parsing entry points; defer until something obvious emerges
- **Benchmark infrastructure** (`criterion`, `bencher`) — performance is not a current concern for tisvlay
- **`cargo-deny` / `cargo-audit`** — separate concern from testing; tracked elsewhere

## Decisions

1. **Codecov**: pre-written, commented out. Enable with a one-line uncomment once the repo is public.
2. **Mock backend**: published to crates.io alongside the other tisvlay crates, gated behind a `mock` Cargo feature. Default-off in release, default-on in workspace dev-deps.
3. **Parameterized tests**: `rstest` as its own phase, complementing proptest.
4. **Coverage baseline**: measured in phase 1, used as the ratchet floor. No arbitrary target.

## Phase A & B design decisions

1. **Mock discovery fixture format**: `MockDiscoveryConfig` struct, each test builds its own registry via a `create_test_registry(cfg)` helper. No shared state means no serialization point, tests are fully parallel by construction. `create_registry()` (production) stays simple and registers an empty-discovery mock. This rules out env vars, global mutexes, and on-disk fixtures.

2. **Pairing failure modes**: per-TV, read from the vendor config as `[tv.mock] pairing_mode = "accept" | "reject" | "timeout" | "delayed_accept:500ms"`. One test can set up two TVs with different modes and exercise both in one flow.

3. **D-Bus bus strategy**: use **private D-Bus instances** via `DBUS_SYSTEM_BUS_ADDRESS` and `DBUS_SESSION_BUS_ADDRESS` environment variables, launched per test binary via a test helper.

   Key insight: `zbus::Connection::system()` and `::session()` respect these env vars. If we launch a private `dbus-daemon` at a unique socket path and set the env var before the daemon connects, the daemon's "system bus" connection transparently lands on our test bus. It runs the *exact same code path* as production — no mocking of the zbus connection, no divergence.

   This lets us test system-event-driven flows (logind `PrepareForSleep`, NM `StateChanged`) by registering a fake logind/NM on our private system bus and emitting signals at it. No root, no polkit, no real hardware.

4. **Parallelism for D-Bus tests**: **private bus per test binary, unique service names per test within the binary.**

   - Each integration test file (`tests/dbus_contract.rs`, `tests/session_events.rs`, etc.) is its own binary
   - Each binary has a lazy static `TEST_BUS` that spawns one `dbus-daemon` at first use (~100ms, amortized across all tests in the binary)
   - Each test uses a unique service name (`org.tisvlay.test.<test_name>.Daemon`) so tests within the binary run parallel
   - Nextest parallelizes across binaries automatically
   - Tests that truly need isolation (watcher tests, daemon-crash-during-signal-watch tests) spawn their own dedicated bus and run in a serial nextest group

   **Required API additions**: `DaemonClient` and the daemon's `connection::Builder` need to accept a custom service name as an optional parameter. Production code uses the default `DAEMON_SERVICE` constant; test helpers override it. `StateWatcher`'s `NameOwnerChanged` watches also need the custom name.

5. **Phase order**: Phase A first, then Phase B. Phase A is small and self-contained; Phase B's D-Bus tests build on the pairing work.

## Implementation order for A & B

1. **Phase A.1**: `MockDiscovery` + `MockDiscoveryConfig` + `create_test_registry(cfg)` helper
2. **Phase A.2**: `MockPairingMode` per-TV via vendor config
3. **Phase A.3**: Integration tests in `tisvlay-backends/tests/mock_discovery.rs` covering all pairing scenarios
4. **Phase A.4**: CLI pair snapshot test (`insta-cmd`) in `tisvlay-cli/tests/`
5. **Phase B.1**: Private D-Bus test harness helper (`crates/tisvlayd/tests/common/dbus.rs`)
6. **Phase B.2**: API additions for custom service names on `DaemonClient`, daemon `connection::Builder`, and `StateWatcher`
7. **Phase B.3**: `TvManager` harness + profile dispatch tests (biggest coverage win for `tv_manager.rs`)
8. **Phase B.4**: `Automated` gate composition tests
9. **Phase B.5**: D-Bus contract tests (Tv1.* interface surface, `PropertiesChanged` signal emission)
10. **Phase B.6**: Session service tests with fake logind on the private system bus
11. **Phase B.7**: `StateWatcher` tests (tray layer) against a daemon backed by mocks

## Implementation order for A & B

1. **Phase A.1**: `MockDiscovery` + `create_registry` wiring (smallest increment)
2. **Phase A.2**: `MockPairingMode` + per-mode tests
3. **Phase A.3**: CLI pair snapshot test (`insta-cmd`)
4. **Phase B.1**: `TvManager` harness + profile dispatch tests (biggest coverage win)
5. **Phase B.2**: `Automated` gate composition tests
6. **Phase B.3**: D-Bus contract tests (most complex, most valuable for refactoring safety)
7. **Phase B.4**: Session service + tray integration tests
