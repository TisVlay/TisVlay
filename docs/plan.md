# Plan: Testing & Measurement Framework

> For the current state of the testing framework (how to use it,
> architecture decisions, harness APIs), see [testing.md](testing.md).
> This document tracks **remaining work only**.

## Completed phases (summary)

| Phase | What it delivered |
|-------|-------------------|
| 1 | Coverage measurement (`cargo-llvm-cov`, ratchet in CI) |
| 2 | `cargo-nextest` with `default` and `ci` profiles |
| 3 | `insta` snapshot tests for `TvInfo` wire format |
| 4 | `rstest` parameterized tests for `tv_path` |
| 5 | `proptest` property tests for `tv_path` + `MacAddress` |
| 6 | Mock backend skeleton (`tisvlay-mock`) |
| A | `MockHandle` for test-driven discovery + pairing |
| B.0 | `tisvlay-test-support` crate (PrivateBus, FakeLogind, FakeScreenSaver, tracing re-exports) |
| B.0 | `DaemonClient::from_connection`, `StateWatcher::from_connections`, `BusKind` enum |
| B.0 | `tracing-test` (with `no-env-filter`), `tracing-mock` in workspace dev-deps |
| B.0 | `tokio/test-util` in tisvlayd dev-deps; clock spike validated |
| B.1 | `tisvlayd` lib+bin refactor, `TestDaemon` harness, profile dispatch / automated gate / multi-TV tests |
| B.2 | D-Bus contract tests (property cache, command round-trips, PropertiesChanged) |
| B.4 | `StateWatcher` tests on private bus |
| B.5 | `tracing-mock` observability regression tests |
| C.1 | `tisvlay-session` lib+bin refactor, `TestSession` harness, `FakeScreenSaver` |
| C.2 | Session service integration tests (lock/unlock/idle dispatch, Session1 D-Bus contract) |

## Remaining work

### Phase 7 — Codecov upload (blocked: repo is private)

Uncomment the upload block in `.github/workflows/ci.yml` once the repo goes public. One-line change; everything else is pre-wired.

### Phase C.3 — CLI pair snapshot test

Snapshot the `tisvlay pair` CLI output via `insta-cmd`.

**Shape:** `TestDaemon` runs in-process on the private bus. The CLI binary is spawned as a subprocess with `DBUS_SESSION_BUS_ADDRESS` set to the test bus address. Output is snapshotted via `insta-cmd`.

**File:** `crates/tisvlay-cli/tests/pair_snapshot.rs`

**Prerequisite:** `insta-cmd` added to workspace dev-deps. TestDaemon already supports pairing round-trips via MockHandle.

**Why deferred:** The CLI subprocess needs the daemon available on a bus address it can reach via env var. This is straightforward with the existing PrivateBus + TestDaemon infrastructure but hasn't been prioritized over higher-coverage items.

## Out of scope (for now)

- **Mutation testing** (`cargo-mutants`) — worth running once coverage stabilizes
- **Fuzzing** (`cargo-fuzz`) — needs clear input-parsing entry points
- **Benchmark infrastructure** (`criterion`, `bencher`) — not a current concern
- **`cargo-deny` / `cargo-audit`** — tracked separately from testing

## Design decisions

Rationale for major framework choices is documented in the "Architecture decisions" section of [testing.md](testing.md#architecture-decisions). The key decisions:

1. Private dbus-daemon per test binary (not per test)
2. `std::sync::Mutex` for MockState (not tokio)
3. Rust-native fakes (not Python dbusmock)
4. `from_connection` constructors (not feature flags)
5. Handle wins over config shorthand
6. `no-env-filter` on tracing-test
