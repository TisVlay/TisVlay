# Plan: Testing & Measurement Framework

> **Note:** This document is the historical implementation plan. For
> the current state of the testing framework (how to use it, architecture
> decisions, harness APIs), see [testing.md](testing.md).

## Goal

Establish an extensible testing and measurement framework for the tisvlay workspace. The framework should:

1. Make it cheap to add new tests (low friction → more tests get written)
2. Catch regressions before they hit production
3. Measure code coverage and treat it as a property to monitor over time
4. Support LLM-assisted development (clear conventions, fast feedback, structured outputs)
5. Run in CI on every PR with results visible inline
6. Lay the groundwork for a mock backend that enables testing high-level features (pairing, discovery, profiles, CLI, GUI) without real hardware

## Status

**Phases 1–6, A, and B complete.** The framework is in place:
- Ratchet coverage at **36.47% lines** (baseline bumped from 19% → 36%), up from 20% pre-Phase A
- 133 tests pass in <1s via nextest; JUnit XML published via dorny/test-reporter
- insta snapshots pin the `TvInfo` wire format
- rstest converts three flat test functions into 7 labelled cases
- proptest runs ~1,800 randomized inputs across 7 properties
- tisvlay-mock crate implements the full `TvBackend` / `TvConnector` / `TvConnection` surface with push-state subscriptions, plus `MockHandle` for test-driven discovery/pairing
- tisvlay-test-support crate provides private `dbus-daemon` per test binary, fake logind, `#[traced_test]` / `tracing-mock` re-exports
- tisvlayd refactored to lib+bin with `TestDaemon` harness, `BusKind::Address` for private bus tests
- `tv_manager.rs` went from 0% → 30% coverage via profile dispatch, automated gate, and multi-TV concurrency tests
- D-Bus contract tests verify property cache, command round-trips, and `PropertiesChanged` emission
- StateWatcher tests verify daemon name-watch lifecycle on a private bus
- tracing-mock observability regression tests pin `#[instrument]` span names and event fields

**Phase 7 deferred** until the repo becomes public (the only remaining work is uncommenting the Codecov upload block).

**Phase C** picks up the two deferred items from Phase B that need additional infrastructure:
- **C.1** — `tisvlay-session` lib extraction + `TestSession` harness (same lib+bin pattern as tisvlayd)
- **C.2** — Session service integration tests (formerly B.3.1)
- **C.3** — CLI pair snapshot test via `insta-cmd` (formerly B.2.2)

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

### Phase A — Discovery + pairing user story ✅ complete

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

### Phase B — Daemon integration tests ✅ complete (B.2.2, B.3.1 deferred)

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

### 1 + 2. Unified `MockHandle` for discovery and pairing state

Both discovery and pairing are test-driven state-machine flows; they share the same architectural shape. Rather than a discovery config struct on one side and a simulate-user-action helper on the other, expose a single **`MockHandle`** that tests use to drive *both*:

```rust
// Test helper, not exported in production builds.
pub fn create_test_registry() -> (BackendRegistry, MockHandle);

impl MockHandle {
    // Discovery (Phase A.1)
    pub fn publish_tv(&self, tv: DiscoveredTv);
    pub fn clear_discovered(&self);

    // Pairing (Phase A.2/A.3)
    pub fn simulate_user_accept(&self, tv_id: &str);
    pub fn simulate_user_reject(&self, tv_id: &str);
    pub fn simulate_pairing_timeout(&self, tv_id: &str);
}
```

Each test:

```rust
let (registry, mock) = create_test_registry();
mock.publish_tv(DiscoveredTv::new("lr", "Living Room", "mock://lr", "mock"));

let tvs = registry.discover_tvs("mock", Duration::from_secs(1)).await?;
// ... start pairing, then mock.simulate_user_accept("lr") to drive it ...
```

**Why this shape**:

- **Symmetric mental model** — one handle, same idiom for discovery and pairing state. No "config struct here, test method there" split.
- **Dynamic scenarios** become trivial: "discover, TV appears mid-session, re-discover" is natural with `publish_tv` called between calls; impossible with a static config.
- **Fully parallel** — each test gets its own `(registry, handle)` pair. The handle holds an `Arc<Mutex<MockState>>` scoped to that registry instance. No global state, no env vars, no serialization points.
- **Matches the research pattern** — Home Assistant's config_flow tests, bluez's simple-agent, and Matter's chip-tool all use injection at test time, not upfront config. The handle is the idiomatic shape for stateful test doubles in this space.

**Config shorthand** on top of the handle — for tests that don't need fine-grained control, the vendor config accepts a pairing shorthand:

```toml
[tv.mock]
address = "mock://living_room"
pairing = "accept"   # or "reject" | "timeout" | "delayed_accept:500ms"
```

The shorthand auto-schedules the corresponding `simulate_user_*` call on a fixed timer, so simple tests don't have to touch the handle at all. Tests that need scripted sequences use the handle directly.

**Under the hood**, pairing is modeled as a small explicit state machine (`Idle → Prompted → {Accepted, Rejected, TimedOut}`). This matches LG's real two-phase `Response` → `Registered` flow and makes future scenarios ("accept, then disconnect after 2s, then reject re-pair") a matter of richer timeline syntax rather than new config keywords.

**Deterministic timing** via `tokio::time::pause()` + `tokio::time::advance()`. No real sleeps in tests. No randomness.

**Phase A scope**: `MockHandle` with discovery publish/clear, pairing accept/reject/timeout helpers, and the `"accept"` / `"reject"` / `"timeout"` config shorthand. Defer richer timeline syntax and inline TOML tables until a concrete test needs them.

**What we skip**: the `fail` crate (overkill for a mock), randomized failures (belong in fuzz/soak, not unit tests), static `MockDiscoveryConfig` structs (superseded by the handle), env var fixtures (not parallel-safe).

**Production registry** stays simple: `create_registry()` registers a mock whose handle is never surfaced externally, so production code cannot accidentally drive the mock's test hooks. Only `create_test_registry()` returns a usable `MockHandle`.

**Implementation notes**:

- `MockState` is a struct wrapped in `std::sync::Mutex` (not `tokio::sync::Mutex`), held behind an `Arc` shared between `MockConnector` and `MockHandle`. The connector must never hold the lock across an `.await` — brief critical sections only. Tests call handle methods synchronously between awaits, so deadlock is structurally avoided.
- **Handle vs shorthand precedence**: if a test both sets `pairing = "accept"` in the TOML and calls `mock.simulate_user_reject(...)` before the shorthand's auto-timer fires, the handle wins — the first explicit transition on the pairing state machine locks in the outcome. The shorthand is sugar on top of the handle, not an independent path.

### 3. D-Bus bus strategy — private `dbus-daemon` per test binary (Option A)

Based on research of upower, iwd, power-profiles-daemon, switcheroo-control, and the zbus test suite:

- **Private `dbus-daemon` per test binary**, amortizing the ~15-40ms spawn cost across all tests in the binary via a `once_cell::sync::Lazy` with a `Drop` guard.
- Spawn with `dbus-daemon --session --print-address`, parse the Unix socket path off stdout, store in env vars (`DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`) before any test code connects.
- **Each bus hosts both "system" and "session" roles** — the test binary tells the daemon "this is your system bus" by setting `DBUS_SYSTEM_BUS_ADDRESS` to the same address. The daemon code is unchanged; it still calls `zbus::Connection::system()` but lands on the private bus. Fake logind/NM are registered on the same bus and emit signals at it.
- **Rust-native fakes**, not Python `dbusmock`. The zbus `ObjectServer::at()` API is enough to hand-roll fake `org.freedesktop.login1.Manager` and `org.freedesktop.NetworkManager` objects that emit the signals we care about. Keeps the test toolchain Rust-only.

This lets us test system-event-driven flows (logind `PrepareForSleep`, NM `StateChanged`) end-to-end — the daemon runs the exact same code path as production, no zbus mocking, no divergence. No root, no polkit, no real hardware.

### 4. Parallelism — binary-level isolation, avoid zbus's connection cache

Critical gotcha from the research: **`zbus` caches `Connection::session()` and `::system()` per-process via `OnceCell`**. Two tests in the same binary that try to point at different buses will silently reuse the first one's connection. This breaks the "one shared bus, unique service names" approach.

The fix has two parts:

1. **One `dbus-daemon` per test binary** (not per test). Tests within a binary share the bus. Parallelism *across* binaries is handled by nextest's process-per-binary model.
2. **Production code must use `connection::Builder::address(...)` explicitly** in test contexts instead of `::session()` / `::system()`. The `Builder::address` path bypasses the cache. This means adding test-friendly connection constructors to our production code or gating the cache-bypassing path behind a `#[cfg(test)]` helper.

**Within a binary**, tests run serialized by default (nextest still uses one thread per test binary when tests share state like a bus). For tests that do not touch the bus or use unique object paths under a shared service name, we can run them in parallel.

**Decisive argument**: name ownership is process-global on a bus. Two tests in parallel that both want to own `org.tisvlay.Daemon` cannot coexist on one bus — one blocks the other. Binary-level isolation sidesteps this entirely by giving each binary its own bus. Unique service names within a shared bus also work, but they don't fix the zbus caching issue, so they're not the primary mechanism.

**Additional safety rails**:
- Scrub `DBUS_STARTER_*` env vars before spawning the test `dbus-daemon` (they leak from the user session and confuse service activation)
- Use `Command::new("dbus-daemon").process_group(0)` to isolate the daemon from `cargo test`'s signal handling (SIGPIPE under the thread pool can silently kill it)
- `Drop` guard on the bus wrapper kills the daemon cleanly on test-binary exit

### 5. Phase order — A first, then B

Phase A is small, self-contained, and exercises the mock backend in a way that validates its API before Phase B leans on it harder. Phase B's D-Bus contract tests need pairing to work, so the `MockPairingMode` work is a prerequisite.

### 6. `DaemonClient` / `StateWatcher` / daemon setup — `from_connection` constructor

Rather than adding a testing-specific feature flag or a `doc(hidden)` escape hatch, expose an idiomatic general-purpose constructor:

```rust
impl DaemonClient {
    pub fn new_session() -> Result<Self> { /* existing */ }
    pub fn new_system()  -> Result<Self> { /* existing */ }

    /// Build a `DaemonClient` from a pre-configured `zbus::Connection`
    /// and a custom service name. Useful for tests (which spawn a private
    /// `dbus-daemon` and connect via `zbus::connection::Builder::address`)
    /// and for custom deployments with alternate service names.
    pub fn from_connection(conn: zbus::Connection, service_name: &str) -> Result<Self> { /* new */ }
}
```

Why this shape:
- **No dead code risk** — the constructor is legitimately useful outside tests (alternate deployments)
- **No feature flag** — `tisvlay-core` stays simple
- **Idiomatic Rust** — delegates connection construction to zbus's own builder, we just accept what it returns
- **Side-steps zbus's `OnceCell` cache** — `zbus::connection::Builder::address(...)` doesn't go through the cached `session()` / `system()` helpers

Same pattern for `StateWatcher::from_connection(conn, daemon_service_name)` and for the daemon's D-Bus registration in `tisvlayd/src/main.rs`.

### 7. Shared test helpers live in a dedicated `tisvlay-test-support` crate

Rather than extending `tisvlay-mock` with an optional `testing` feature and later refactoring it out, create `crates/tisvlay-test-support/` up front.

Responsibilities:
- Private `dbus-daemon` spawner with `Lazy` + `Drop` guard
- Hand-rolled fake `org.freedesktop.login1.Manager` (`PrepareForSleep` signal emission)
- Hand-rolled fake `org.freedesktop.NetworkManager` — actually, **skip this**. We removed the NM watcher in the networking branch; carrier/route events come from netlink and we don't fake those in tests (validated via `bouncing_routes.py` telemetry).
- Test-binary-scoped environment scrubbing (`DBUS_STARTER_*`)
- `tokio::process::Command` wrapper with `process_group(0)` for signal isolation
- Integration with `tracing-test` for per-test log capture (see §8)

Dependencies (dev-only throughout the workspace, never shipped in release):
- `tempfile` for unique socket paths
- `tokio::process` + `tokio::io` for the dbus-daemon child
- `zbus` for registering the fake services
- `tracing`, `tracing-subscriber`, `tracing-test`, `tracing-mock` for observability

Why this shape:
- **Clean separation of concerns** — `tisvlay-mock` remains "mock backend implementation"; test-support remains "test harness for the whole workspace"
- **Avoids the "extract from mock" refactor later** — honors the user's requirement not to refactor test infrastructure
- **Honest dependency graph** — each consumer (`tisvlayd/tests`, `tisvlay-session/tests`, `tisvlay-core/tests`) dev-depends on `tisvlay-test-support`; no hidden coupling through `tisvlay-mock`

### 8. `tracing` integration for test observability

When integration tests fail, bare `assert_eq!` messages are rarely enough to diagnose what went wrong. The `tracing` ecosystem has two crates built for this:

- **`tracing-test`** — the `#[traced_test]` attribute macro installs a per-test subscriber that captures `tracing::info!` / `debug!` / etc. calls. On failure, captured logs are dumped to stderr alongside the assertion error. On success, discarded. Zero config.
- **`tracing-mock`** — build an expected-event / expected-span pattern, run code under it, assert the pattern matched. Lets us test that production code is instrumented correctly (regression protection for observability).

**Concrete uses**:

1. **Test-infra self-observability**: `MockHandle` methods, the D-Bus harness, the state-machine transitions — all get `#[instrument]` attributes. When a test fails, the captured log shows every step of "the test published 2 TVs, started pairing on the wrong ID, got a not-found error".

2. **Production code coverage**: the daemon and session service already emit tracing events throughout (that's what we've been using for debugging). Tests automatically get those for free via `tracing-test`.

3. **Observability regression tests**: a small set of `tracing-mock`-based tests assert that key events (e.g., `"Route changed: reconnecting"` in `tv_manager.rs`) are emitted with the right fields. Catches regressions where someone refactors and silently removes a log line.

**Pattern** (every Phase B test uses this):

```rust
#[traced_test]
#[tokio::test]
async fn on_session_lock_dispatches_screen_off() {
    let harness = TestHarness::new().await;
    harness.add_tv(mock_config_with_profile("default")).await;

    harness.emit_session_lock().await;
    harness.wait_for_action_dispatch().await;

    // Assertions on state
    assert_eq!(harness.tv_state("lr").await.is_screen_on, false);
    // tracing-test captured logs are automatically dumped if this fails,
    // showing the full event dispatch chain.
}
```

**Dependencies** (workspace dev-deps, dev-only throughout):

```toml
[workspace.dependencies]
tracing-test = "0.2"
tracing-mock = "0.1"  # currently pre-1.0 but stable enough
```

**Documentation**: `.claude/CLAUDE.md` gets a "Testing with tracing" subsection explaining the `#[traced_test]` pattern and when to use `tracing-mock` for assertions.

## Implementation order for A & B

### Phase A — Discovery + pairing (mock-level, no D-Bus) ✅ complete

1. **A.1**: `MockState` struct + `MockHandle` + `Arc<Mutex<MockState>>` wiring between `MockConnector` and the handle. Lock is `std::sync::Mutex`; connector never holds it across an `.await`.
2. **A.2**: `MockDiscovery` reads from `MockState`; test helper `create_test_registry()` returns `(BackendRegistry, MockHandle)`.
3. **A.3**: Internal pairing state machine driven by `simulate_user_*` methods; config shorthand (`"accept"` / `"reject"` / `"timeout"`) wraps the handle on a fixed timer for simple tests.
4. **A.4**: Integration tests in `tisvlay-backends/tests/mock_discovery.rs` — happy path, reject, timeout, multiple TVs, empty network, mixed-mode multi-TV, dynamic publish-after-discover.
5. **A.5**: Document `MockHandle` and the mock pairing API in `.claude/CLAUDE.md`.

### Phase B — Daemon integration tests ✅ complete (except B.2.2, B.3.1 → Phase C)

**B.0 — Shared test infrastructure** ✅

1. **B.0.1** — `crates/tisvlay-test-support/` with private `dbus-daemon` spawner, `FakeLogind`, `tempfile`-backed socket paths, `tracing-test`/`tracing-mock` re-exports.
2. **B.0.2** — `DaemonClient::from_connection(conn)` and `StateWatcher::from_connections(daemon_conn, session_conn, config)` in `tisvlay-core`. Daemon-side `BusKind` extraction rolled into B.1.1.
3. **B.0.3** — `tracing-test` (with `no-env-filter`), `tracing-mock` in workspace dev-deps. `#[traced_test]` pattern documented in CLAUDE.md.
4. **B.0.4** — `tokio/test-util` in `tisvlayd` dev-deps.
5. **B.0.5** — Clock spike confirms `start_paused = true` + `advance()` works with mpsc + sleep patterns.

**B.1 — `TvManager` integration** ✅

6. **B.1.1** — `tisvlayd` refactored to lib+bin with `TestDaemon` harness and `BusKind::Address` for private bus tests.
7. **B.1.2** — Profile dispatch tests (`on_startup` through `on_no_session`). `tv_manager.rs` went from 0% → 30%.
8. **B.1.3** — `Automated` gate composition tests (managed × automate_when × primary_input × current_input).
9. **B.1.4** — Multi-TV concurrency tests.

**B.2 — D-Bus contract tests** ✅ (B.2.1 done; B.2.2 deferred to Phase C)

10. **B.2.1** — Property cache, command round-trips, `PropertiesChanged` emission across full `Tv1.*` surface.
11. ~~**B.2.2**~~ → **C.3** (CLI pair snapshot — needs daemon-as-subprocess harness).

**B.3 — System-event tests** (B.3.1 deferred to Phase C)

12. ~~**B.3.1**~~ → **C.2** (session service tests — needs `tisvlay-session` lib extraction).

**B.4 — `StateWatcher` tests** ✅

13. **B.4.1** — `StateWatcher` tests on private bus with daemon name-watch lifecycle.

**B.5 — `tracing-mock` observability regressions** ✅

14. **B.5.1** — A small set of tests that use `tracing-mock` to assert on specific production events: the daemon logs a `"Route changed"` event with the right fields, `"Monitored interface updated"` fires with `tv_id` and `previous`/`current`, etc. Catches refactors that silently remove observability.

### Phase C — Deferred items: session service tests + CLI snapshot

**Goal**: close the two gaps left from Phase B that require additional structural work — the session service lib extraction and the CLI pair snapshot test.

**Prerequisites**: Phases A and B complete (which they are).

**Shared code opportunity**: `tisvlayd` and `tisvlay-session` share two patterns worth extracting:

| Pattern | tisvlayd | tisvlay-session | Target |
|---------|----------|-----------------|--------|
| `BusKind` enum (Session/System/Address) | `dbus/setup.rs` | hardcoded `::session()` | `tisvlay-core` or `tisvlay-test-support` |
| `wait_for_signals` loop (SIGTERM/SIGINT/SIGHUP) | `main.rs` | `main.rs` | `tisvlay-core` |

Moving `BusKind` to a shared location lets both services adopt it without duplication. The signal handler loop is nearly identical in both mains — extracting it eliminates a maintenance footprint.

**C.1 — `tisvlay-session` lib extraction**

1. **C.1.1** — Add `crates/tisvlay-session/src/lib.rs` with `pub mod` for existing private modules (`session_dbus`, `session_monitor`, `idle`, `wayland_idle`). Move `SessionState` and event-handler logic into the library so tests can drive them.
2. **C.1.2** — Update `main.rs` to be a thin entry point importing from the library (same pattern as `tisvlayd/src/main.rs`).
3. **C.1.3** — Add `BusKind` support to `register_session_dbus()` so tests can point at a private bus via `BusKind::Address(...)`. Consider whether `BusKind` should live in `tisvlay-core` (shared between both services) or stay in each service's `dbus/setup.rs`.
4. **C.1.4** — Add `FakeScreenSaver` to `tisvlay-test-support` (same pattern as `FakeLogind`) — hand-rolled `org.freedesktop.ScreenSaver` stub with `ActiveChanged` signal emission for lock/unlock simulation.
5. **C.1.5** — Build `TestSession` harness analogous to `TestDaemon`: spins up the session service against a `TestDaemon` on the shared private bus. Returns handles for driving events and asserting state.

**C.2 — Session service integration tests** (formerly B.3.1)

6. **C.2.1** — Session event tests: inject `PrepareForSleep(true)` via `FakeLogind`, lock/unlock via `FakeScreenSaver`, idle timeout, and assert `SessionAction` dispatch on mock TVs. Verify user profile resolution and event replay paths.
7. **C.2.2** — Session D-Bus contract: drive `org.tisvlay.Session1` methods (`ListUserProfiles`, `SetTvUserProfile`, etc.) from a client on the test bus, assert signal emission.

**C.3 — CLI pair snapshot test** (formerly B.2.2)

8. **C.3.1** — CLI pair snapshot in `crates/tisvlay-cli/tests/pair_snapshot.rs` using `insta-cmd`. `TestDaemon` runs in-process on the private bus. The CLI binary is spawned as a subprocess with `DBUS_SESSION_BUS_ADDRESS` set to the test bus address. Output is snapshotted via `insta-cmd`.

**Estimated effort**: C.1 is the bulk (lib extraction + `FakeScreenSaver` + `TestSession`). C.2 and C.3 follow mechanically once C.1 is done.
