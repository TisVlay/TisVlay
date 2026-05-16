# Testing

This is the single source of truth for how testing works in tisvlay. For quick-reference commands, see the `Tasks & Commands` section in [CLAUDE.md](../.claude/CLAUDE.md). For architecture and D-Bus interface specs, see [design.md](design.md).

## Overview

The testing framework is designed around six principles:

1. **Low friction** — adding a test should be cheap so more tests get written
2. **Regression safety** — catch bugs before they hit production
3. **Coverage as a ratchet** — new code must not lower the baseline
4. **LLM-friendly conventions** — predictable layout, fast feedback, structured output
5. **CI visibility** — results inline on every PR
6. **No real hardware required** — the mock backend enables testing pairing, discovery, profiles, and UX without a TV on the network

## Test runner: cargo-nextest

All compiled tests run through [cargo-nextest](https://nexte.st/). Doc tests use `cargo test --doc` (nextest doesn't support them yet).

```bash
just test         # nextest + doc tests
just test-ci      # nextest with 'ci' profile + coverage + ratchet
```

Configuration: `.config/nextest.toml` with two profiles:

| Profile | Retries | Slow warning | Output |
|---------|---------|--------------|--------|
| `default` | 0 | 60s | terminal |
| `ci` | 1 | 30s | JUnit XML at `target/nextest/ci/junit.xml` |

**Process-per-test isolation** matters because daemon code uses global state (D-Bus well-known names, netlink sockets, config file paths). `cargo test`'s shared-process model would have tests stepping on each other.

### Test execution pipeline

```mermaid
flowchart LR
    JT["just test"] --> NX["cargo nextest run<br/>--workspace --all-features"]
    JT --> DT["cargo test --doc"]
    NX --> R{{"tests<br/>(process-per-test)"}}
    DT --> D{{"doc tests"}}

    JTC["just test-ci"] --> COV["cargo llvm-cov<br/>--no-report nextest"]
    COV --> R
    COV --> LCOV["lcov.info"]
    LCOV --> RATCHET{"coverage >= baseline?"}
    RATCHET -->|yes| JUNIT["junit.xml<br/>(dorny/test-reporter)"]
    RATCHET -->|no| FAIL["CI fails"]
```

## Test types

| Type | Location | Tool | When |
|------|----------|------|------|
| Unit | `#[cfg(test)] mod tests` in source | built-in | every change |
| Integration | `crates/*/tests/*.rs` | built-in | every change |
| Doc | `///` examples in `pub` items | `cargo test --doc` | every change |
| Parameterized | unit/integration | `rstest` | every change |
| Property | unit/integration | `proptest` | every change |
| Snapshot | `crates/*/tests/snapshots/` | `insta` | every change |
| Mock backend | `tisvlay-backends/tests/`, `tisvlayd/tests/` | mock crate | every change |

### Parameterized tests (rstest)

One test function, many inputs. Each case reported individually by nextest.

```rust
use rstest::rstest;

#[rstest]
#[case::simple("living_room", "/org/tisvlay/Tv/living_room")]
#[case::with_dash("living-room", "/org/tisvlay/Tv/living_room")]
#[case::with_space("living room", "/org/tisvlay/Tv/living_room")]
fn tv_path_sanitization(#[case] tv_id: &str, #[case] expected: &str) {
    assert_eq!(tv_path(tv_id), expected);
}
```

### Property tests (proptest)

Discovers unknown failures via random inputs, shrinks to minimal repro.

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn tv_path_suffix_is_valid(tv_id in ".*") {
        let path = tv_path(&tv_id);
        let suffix = path.strip_prefix(&format!("{}/", TV_BASE_PATH)).unwrap();
        prop_assert!(suffix.chars().all(|c| c.is_alphanumeric() || c == '_'));
    }
}
```

When a proptest fails, it writes a `.proptest-regressions` file. **Commit these files** — they replay the failing seed on future runs.

### Snapshot tests (insta)

Compare generated output against saved reference files. Workflow:

1. Write test: `insta::assert_json_snapshot!(value)`
2. `just test` — new/changed snapshots fail, write `.snap.new`
3. `just test-snapshot-review` — interactive diff TUI
4. Commit updated `.snap` files

Snapshots live in `crates/*/tests/snapshots/`.

### CLI snapshot tests (insta-cmd)

`crates/tisvlay-cli/tests/pair_snapshot.rs` snapshots the `tisvlay pair` and `tisvlay list` CLI output. A `TestDaemon` runs in-process on the private bus; the CLI binary is spawned as a subprocess with `DBUS_SESSION_BUS_ADDRESS` pointing at the test bus.

```rust
assert_cmd_snapshot!(Command::new(cli_bin())
    .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
    .arg("--user")
    .arg("pair")
    .arg("mock://living_room")
    .arg("--vendor")
    .arg("mock")
    .arg("--name")
    .arg("Living Room"));
```

These tests use `#[tokio::test(flavor = "multi_thread")]` because `assert_cmd_snapshot!` blocks the current thread with `std::process::Command::output()` — the TestDaemon's D-Bus server tasks need other threads to handle incoming calls from the subprocess.

## LG backend testing

The `tisvlay-lg` crate has its own fake SSAP server for testing without a real TV. See [docs/lg/testing.md](lg/testing.md) for details.

### On-hardware triage tooling

For end-to-end smoke-test latency measurement on real hardware, `scripts/triage_dispatch_perf.py` opens an out-of-band SSAP WebSocket via the daemon's existing `client_key`, sends `ssap://system/turnOff` to put the TV in `screen_off`, then dispatches reliable `screen_on` and times `t0 → PowerState == "active"`. Useful when investigating wake-recovery latency without driving the actual desktop lock/unlock flow. See [docs/lg/firmware-quirks.md](lg/firmware-quirks.md) "The 10 s `REQUEST_MS` is load-bearing" for the spike findings the script was built to characterize.

## Coverage

Measured with `cargo-llvm-cov`, ratcheted in CI.

```bash
just cov          # text summary
just cov-html     # HTML report in browser
just cov-lcov     # lcov.info for VS Code Coverage Gutters
just cov-ratchet  # fails if below baseline (CI uses this)
```

The baseline lives in `justfile` as `cov_baseline_lines`. Bump it on PRs that improve coverage; never lower it. Run `just cov` to see the current number.

CI runs the full check pipeline (`clippy` + `nextest` + doc tests + coverage report) under a single coverage-instrumented build via `cargo llvm-cov show-env --sh`, so all four stages share `target/llvm-cov-target/` instead of recompiling the workspace twice. The mock-only feature subset CI uses lives in `justfile` as `ci_features`; its comment block is the canonical place to document feature-graph trade-offs and trigger conditions for extending the set. Local `prek` still runs `--all-features` clippy via `just clippy`. See [docs/build.md](build.md) "Build pipeline" for design rationale and upstream tracking.

## Mock backend (tisvlay-mock)

Implements the full `TvBackend` / `TvConnector` / `TvConnection` / `TvDiscovery` trait surface with an in-memory state machine. Feature-gated behind `mock` on `tisvlay-backends` (default-off in release, default-on in workspace dev/test builds).

Key properties:
- Reports `Transport::Network` and `survives_source_ip_change() -> false` (same paths as LG)
- `subscribe_state()` returns a real `mpsc::UnboundedReceiver<TvState>` pushing cumulative snapshots on every mutation (mirrors LG's SSAP model)
- Pairing state machine matches LG's two-phase flow

### Shared state architecture

The mock's three consumers share a single `Arc<Mutex<MockState>>`. The handle is the test's control surface; the connector and discovery are the trait implementations the daemon calls.

```mermaid
flowchart TD
    STATE["Arc&lt;Mutex&lt;MockState&gt;&gt;<br/><br/>discovered: Vec&lt;DiscoveredTv&gt;<br/>pairing_outcomes: HashMap&lt;String, PairingOutcome&gt;<br/>live_states: HashMap&lt;String, Arc&lt;Mutex&lt;TvState&gt;&gt;&gt;<br/>subscription_txs: HashMap&lt;String, Vec&lt;Sender&gt;&gt;"]

    HANDLE["MockHandle<br/>(test code)"]
    CONN["MockConnector<br/>(TvConnector trait)"]
    DISC["MockDiscovery<br/>(TvDiscovery trait)"]
    MC["MockConnection<br/>(TvConnection trait)"]

    HANDLE -- "publish_tv()<br/>clear_discovered()<br/>simulate_user_*()<br/>inject_power_state()" --> STATE
    CONN -- "start_pairing()<br/>reads + removes outcome" --> STATE
    CONN -- "connect()<br/>creates connection,<br/>registers state Arc" --> MC
    MC -- "subscribe_state()<br/>registers subscription tx" --> STATE
    DISC -- "discover()<br/>reads discovered list" --> STATE

    REG["BackendRegistry"] --> CONN
    REG --> DISC

    subgraph "create_test_registry()"
        STATE
        HANDLE
        CONN
        DISC
    end
```

### Pairing state machine

The mock mirrors LG's two-phase pairing flow. `PairingOutcome` is seeded by the test (via handle or config shorthand) before `start_pairing()` consumes it.

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> AwaitingPrompt: start_pairing()
    AwaitingPrompt --> Accepted : PairingOutcome Accept
    AwaitingPrompt --> Rejected : PairingOutcome Reject
    AwaitingPrompt --> TimedOut : PairingOutcome Timeout
    AwaitingPrompt --> Cancelled : cancel / channel closed
    Accepted --> [*] : Credentials returned
    Rejected --> [*] : PairingError Rejected
    TimedOut --> [*] : PairingError Timeout
    Cancelled --> [*] : PairingError Cancelled
```

### MockHandle — test-driven control surface

Tests drive discovery and pairing via `MockHandle`:

```rust
let (registry, mock) = create_test_registry();

mock.publish_tv(DiscoveredTv::new("lr", "Living Room", "mock://lr", "mock"));
mock.simulate_user_accept("mock://lr");

let tvs = registry.discover_tvs("mock", Duration::from_secs(1)).await?;
// ... start pairing — it auto-accepts ...
```

**Methods:**
- `publish_tv(DiscoveredTv)` / `clear_discovered()` — populate/clear the "network"
- `simulate_user_accept(tv_id)` / `simulate_user_reject(tv_id)` / `simulate_pairing_timeout(tv_id)` — drive pairing
- `set_hardware_id(tv_id, HardwareId)` / `clear_hardware_id(tv_id)` — seed the identity reported by a future connection (applied on every connect, not consumed)
- `set_connect_delay(Duration)` / `clear_connect_delay()` — simulate a slow/unreachable TV
- `inject_power_state(address, PowerState)` — push a power-state change onto a live mock connection. Mutates the in-flight `TvState` and pushes the snapshot through every active subscription for that address, so the daemon's `state_forwarder` updates the cached `Tv1.Power.PowerState` property the same way it would for a real backend-initiated change. Used by `tisvlay-session/tests/session_events.rs::reliable_screen_on_short_circuits_when_tv_slipped_to_standby` to simulate the TV silently slipping past Screen Off to Standby and assert the daemon's pre-flight short-circuit dispatches `power_on` directly.

**Config shorthand:** tests that don't need fine-grained control can set `[tv.mock] pairing = "accept"|"reject"|"timeout"`. If both shorthand and handle are set, **the handle wins**.

Each test gets its own `(registry, handle)` pair — parallel by construction.

### Running the mock in development

```toml
[[tv]]
uuid = "00000000-0000-4000-8000-000000000001"
name = "Mock Living Room"
vendor = "mock"
managed = true

[tv.mock]
address = "mock://living_room"
initial_volume = 30
initial_input = "HDMI_1"
inputs = [["HDMI_1", "HDMI 1"], ["HDMI_2", "HDMI 2"]]
```

```bash
cargo run --bin tisvlayd --features mock -- --session
```

## Test infrastructure: three layers

Test-related crates are `publish = false` and split by what they fake:

| Crate | Fakes | Example |
|---|---|---|
| `tisvlay-test` | OS environment (the system tisvlay runs on) | `PrivateBus`, `FakeLogind`, `FakeScreenSaver` |
| `tisvlay-mock` | TV protocol peer (fake vendor backend) | `MockConnector`, `MockHandle` |
| `tisvlay-harness` | Binary lifecycle (orchestrates the whole daemon/session process) | `TestDaemon`, `TestSession`, `TestTray` |

`tisvlay-harness` reaches into `tisvlayd` and `tisvlay-session` internals through each binary crate's `#[doc(hidden)] pub mod __internals` re-export surface (same idiom `serde::__private` uses). Those items are explicitly not part of any published API.

### How the three crates relate

Each crate fakes a different *direction* relative to the code under test. The harness sits above (orchestrates a binary), the mock sits across (the TV on the other side of the protocol), and the test crate sits below (the OS environment the daemon runs inside).

```mermaid
flowchart TD
    H["<b>tisvlay-harness</b><br/>caller / orchestrator<br/><i>TestDaemon · TestSession · TestTray</i>"]
    D["<b>tisvlayd / tisvlay-session</b><br/>the daemon under test"]
    M["<b>tisvlay-mock</b><br/>fake TV (peer side)<br/><i>MockConnector · MockHandle</i>"]
    T["<b>tisvlay-test</b><br/>fake OS (env side)<br/><i>PrivateBus · FakeLogind · FakeScreenSaver</i>"]

    H -->|"boots binary<br/>via __internals"| D
    D -->|"TvBackend / TvConnection traits"| M
    D -->|"D-Bus / logind / screensaver"| T
    H -.->|"composes"| M
    H -.->|"composes"| T
```

### Three rules for deciding where new test code lives

1. **Does it implement a `tisvlay-core` trait?** → `tisvlay-mock`. It's a backend, just a fake one — same tier as `tisvlay-lg`, not a harness.
2. **Does it fake a system service we don't own (dbus-daemon, logind, ScreenSaver, etc.)?** → `tisvlay-test`.
3. **Does it orchestrate a binary (`tisvlayd`, `tisvlay-session`, `tisvlay-tray`) and reach into its `__internals`?** → `tisvlay-harness`.

### Why these boundaries, not fewer

**`tisvlay-mock` is separate from `tisvlay-harness`** because mock is library-level and reusable from any test that needs a fake backend — `tisvlay-core` unit tests, `tisvlay-cli` snapshot tests, or a hypothetical downstream consumer's integration tests. Folding it into the harness would tie a useful library-level tool to "spin up the whole daemon" ceremony.

**`tisvlay-test` is separate from `tisvlay-mock`** because they answer different questions. A backend-trait-contract test needs `tisvlay-mock` but no `dbus-daemon`; a service-lifecycle test needs `PrivateBus` + `FakeLogind` but no TV protocol stack. Split crates let each test pull in only what it needs.

## Test utilities: tisvlay-test

Dev-only crate providing shared infrastructure. Never shipped in release.

### PrivateBus

Spawns one private `dbus-daemon` per test binary via `LazyLock`.

```rust
let bus = PrivateBus::shared();
let conn = bus.connect().await?;
```

Key design:
- `PR_SET_PDEATHSIG = SIGKILL` — kernel kills daemon on test crash
- Env scrubbing (`DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`, `DBUS_STARTER_*`) — isolates from user session
- `Builder::address(...)` — bypasses zbus's `OnceCell` connection cache
- Binary-level isolation via nextest's process-per-binary model

### Fake D-Bus services

Hand-rolled stubs for system services the daemon depends on:

| Fake | Real service | Key capability |
|------|-------------|----------------|
| `FakeLogind` | `org.freedesktop.login1.Manager` | `Inhibit` method + `PrepareForSleep` signal |
| `FakeScreenSaver` | `org.freedesktop.ScreenSaver` | `GetSessionIdleTime` + `ActiveChanged` signal |

Register on any bus connection:

```rust
let logind = FakeLogind::register(&conn).await?;
logind.emit_prepare_for_sleep(true).await?;

let ss = FakeScreenSaver::register(&conn).await?;
ss.emit_active_changed(true).await?;  // simulate lock
```

### Tracing re-exports

`tisvlay-test` re-exports `tracing_test::traced_test` and `tracing_mock` so consumers don't need separate dep entries.

## Daemon testing (TestDaemon)

`tisvlay_harness::daemon::TestDaemon` builds a fully wired daemon on the shared private bus. Lives in the `tisvlay-harness` crate; pulled into the daemon crate's `[dev-dependencies]`.

### Test harness wiring

```mermaid
flowchart TD
    subgraph PROCESS["Test binary process"]
        subgraph BUS["PrivateBus::shared()"]
            DBUS["dbus-daemon<br/>unix:path=/tmp/.../bus.sock"]
        end

        subgraph TD["TestDaemon"]
            MGR["TvManager<br/>(SystemConfig + MockConnector)"]
            DBSURF["D-Bus surface<br/>Manager1 + ObjectManager<br/>+ Tv1.* per TV"]
            FWD["state_change_forwarder<br/>(tokio::spawn)"]
            MH["MockHandle"]
        end

        subgraph CLIENT["Test assertions"]
            DC["DaemonClient<br/>::from_connection()"]
        end

        LOGIND["FakeLogind"]
    end

    DBUS <--> DBSURF
    DBUS <--> DC
    DBUS <--> LOGIND
    MGR --> FWD
    FWD --> DBSURF
    MH -.->|"shares Arc&lt;Mutex&lt;MockState&gt;&gt;"| MGR
```

```rust
use tisvlay_harness::daemon::TestDaemon;
use tisvlay_core::DaemonClient;

#[tokio::test]
async fn my_test() {
    let daemon = TestDaemon::with_one_mock_tv("my_tv").await.unwrap();
    let client = DaemonClient::from_connection(
        tisvlay_test::PrivateBus::shared().connect().await.unwrap(),
    ).await.unwrap();

    daemon.manager().reconnect("my_tv").await.unwrap();
    // ... assert via client.list_tvs(), daemon.handle(), etc.
}
```

**Constructors:**
- `with_one_mock_tv(tv_id)` — one managed mock TV, default profile
- `with_config(SystemConfig)` — fully custom config
- `mock_tv_config(tv_id, managed, primary_input)` — helper for SystemConfig

**Holds:** `MockHandle`, `Arc<TvManager>`, `zbus::Connection`. Forwarder task aborted on drop; connection release unregisters the name.

**Integration tests:**
- `tests/test_harness_smoke.rs` — basic wiring
- `tests/profile_dispatch.rs` — system event dispatch
- `tests/automated_gate.rs` — managed x automate_when gate
- `tests/concurrent_tvs.rs` — multi-TV independence
- `tests/dbus_contract.rs` — property cache + command round-trips
- `tests/pair_round_trip.rs` — full pairing flow through D-Bus

### Pairing round-trip flow

This is the full code path exercised by `pair_round_trip.rs`. With the async-spawn AddTv contract, the test subscribes to `PairingProgress` first, calls `AddTv` to get the reserved tv_id immediately, then drains the filtered stream to a terminal phase. The handshake itself + post-handshake setup (config persist, install in `tvs` map, register per-TV D-Bus interfaces) all run in a daemon-side spawned `run_pair_task`; the terminal `complete` only fires after every step finishes, so the test can call into the new TV's `Tv1.*` interfaces immediately after `drain_pair_to_terminal` returns `Complete`.

```mermaid
sequenceDiagram
    participant Test
    participant Handle as MockHandle
    participant Client as DaemonClient
    participant DBus as D-Bus (private bus)
    participant Mgr as TvManager
    participant Task as run_pair_task (spawned)
    participant Conn as MockConnector

    Test->>Handle: simulate_user_accept("mock://lr")
    Note over Handle: seeds PairingOutcome::Accept<br/>in shared MockState

    Test->>Client: pairing_progress_stream()
    Client-->>Test: Stream

    Test->>Client: pair_tv("Living Room", "mock", config, true)
    Client->>DBus: AddTv(name, vendor, json, managed)
    DBus->>Mgr: pair_tv(self_arc, ..., connection.clone())
    Mgr->>Mgr: validate vendor, generate tv_id
    Mgr->>Task: tokio::spawn(run_pair_task(...))
    Mgr-->>DBus: (ObjectPath, tv_id)
    DBus-->>Client: (ObjectPath, tv_id)
    Client-->>Test: Ok(tv_id)

    Task->>Conn: start_pairing(vendor_config)
    Note over Conn: removes pre-seeded Accept<br/>from MockState
    Conn-->>Task: PairingSession (emits non-terminal phases<br/>via state_change_tx → PairingProgress signal)
    Task->>Task: wait_for_completion → Credentials
    Task->>Task: connect, install in tvs map, save config
    Task->>DBus: register_tv_interfaces(tv_id)
    Task->>DBus: emit PairingProgress(tv_id, "complete", ...)

    Test->>Client: drain_pair_to_terminal(stream, tv_id, ...)
    Client-->>Test: PairTerminal::Complete { tv_id, ... }
```

### BusKind and from_connection constructors

`tisvlay_core::BusKind` enum (`Session | System | Address(&str)`) is shared by both services. Tests use `BusKind::Address(bus.address())` to point at the private bus.

`DaemonClient::from_connection(conn)` and `StateWatcher::from_connections(daemon_conn, session_conn, config)` bypass the zbus `OnceCell` cache so each test gets an isolated connection.

## Session testing (TestSession)

`tisvlay_harness::session::TestSession` wires a `TestDaemon` + `DaemonClient` + `SessionState` + `FakeScreenSaver` + event handler on the shared private bus. Lives in the `tisvlay-harness` crate.

```rust
use tisvlay_harness::session::TestSession;
use tisvlay_session::session_monitor::SessionEvent;

#[tokio::test]
async fn session_lock_dispatches_screen_off() {
    let session = TestSession::new("my_tv").await.unwrap();
    session.inject_event(SessionEvent::SessionLocked);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let tvs = session.client().list_tvs().await.unwrap();
    assert!(!tvs[0].is_screen_on);
}
```

**Methods:**
- `inject_event(SessionEvent)` — push directly into event handler (deterministic)
- `screensaver()` — access FakeScreenSaver for round-trip signal tests
- `client()` / `daemon()` / `state()` — access internals for assertions

**Integration tests:**
- `tests/session_events.rs` — lock/unlock/idle/greeter dispatch
- `tests/session_dbus_contract.rs` — Session1 interface methods + signals

### Session event dispatch flow

This is the round-trip exercised by `session_events.rs` when a `SessionLocked` event is injected.

```mermaid
sequenceDiagram
    participant Test
    participant EH as event_handler
    participant Client as DaemonClient
    participant DBus as D-Bus (private bus)
    participant Mgr as TvManager
    participant Mock as MockConnection
    participant Fwd as state_change_forwarder

    Test->>EH: inject_event(SessionLocked)
    EH->>Client: list_tvs()
    Client->>DBus: GetManagedObjects
    DBus-->>Client: TV list (filter automated)
    EH->>EH: resolve profile → on_session_lock = {ScreenOff, qos}
    EH->>Client: dispatch_action(tv_id, ScreenOff, qos)
    Client->>DBus: Tv1.Dispatch("screen_off", qos_json)
    DBus->>Mgr: dispatch_action(tv_id, ScreenOff, qos)
    Mgr->>Mock: screen_off()
    Mock->>Mock: update TvState, push to subscriber
    Mock-->>Fwd: TvState snapshot via mpsc
    Fwd->>DBus: update cached property + PropertiesChanged
    Note over Test: settle() — wait 50ms for<br/>D-Bus property propagation
    Test->>Client: list_tvs()
    Client->>DBus: GetManagedObjects
    DBus-->>Client: is_screen_on = false
    Test->>Test: assert!(!tv.is_screen_on)
```

## GUI testing (TestGui + i-slint-backend-testing)

The `tisvlay-gui` test surface has two layers, mirroring the broader Slint ecosystem split between unit-style element-tree assertions and live-bus end-to-end runs.

**Layer B — `tisvlay-gui/tests/`**: instantiate an `AppWindow` against synthetic models, query the element tree via `i_slint_backend_testing::ElementHandle`. Uses `init_no_event_loop()` — multiple `#[test]` functions per binary; no D-Bus, no daemon, no event loop. Best for "does the UI render the right thing for this state?" assertions.

```rust
use slint::{ModelRc, SharedString, VecModel};
use tisvlay_gui::{AboutComponentRow, AppWindow, Pane};

#[test]
fn diagnostics_button_announces_its_label() {
    i_slint_backend_testing::init_no_event_loop();
    let app = AppWindow::new().unwrap();
    app.set_about_components(ModelRc::new(VecModel::from(vec![/* synthetic rows */])));
    app.set_active_pane(Pane::About);

    let elem = ElementHandle::find_by_accessible_label(&app, "Diagnostics").next().unwrap();
    assert!(elem.is_valid());
}
```

**Layer C — `tisvlay-harness/tests/gui_*.rs`**: `TestGui` composes a [`TestSession`](#session-testing-testsession) with the Slint testing backend so end-to-end tests drive the daemon through real D-Bus and assert against the resulting UI state. Uses `init_integration_test_with_system_time()` — Slint panics if called twice in a process, so **one `#[test]` per integration-test binary**. Author new e2e scenarios as sibling files (`gui_pair_flow.rs`, future `gui_remove_flow.rs`, etc.), not additional `#[test]` functions in one file.

```rust
use tisvlay_gui::__internals::{SnapshotInputs, convert_snapshot};
use tisvlay_gui::AppWindow;
use tisvlay_harness::gui::TestGui;

#[test]
fn paired_tv_renders_in_master_list() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let gui = TestGui::with_one_mock_tv("Living Room").await.unwrap();
        let tvs = gui.session().client().list_tvs().await.unwrap();
        let snapshot = convert_snapshot(SnapshotInputs { tvs, ..Default::default() });
        let app = AppWindow::new().unwrap();
        // ... push snapshot, assert via app.get_tvs()
    });
}
```

### Required build env

Slint's compiler embeds element-id metadata only when `SLINT_EMIT_DEBUG_INFO=1` is set **at build time** (when `slint_build::compile` runs in `build.rs`). Without it, `find_by_element_id` / `accessible_id()` silently return nothing and tests look like they "found no elements."

The repo sets this once in `.cargo/config.toml [env]`, which Cargo reads from the workspace root for every invocation — `just build`, bare `cargo test`, rust-analyzer, IDE check-on-save, CI workflow steps. The justfile deliberately doesn't carry it because that would only apply to cargo invocations made via `just` (and would silently break direct `cargo` calls during local dev).

### Naming conventions

- `accessible-label` (translated via `@tr(...)`) — the screen-reader announcement; defaults to the visible text on button primitives. Tests query via `ElementHandle::find_by_accessible_label`.
- `accessible-id` (literal, untranslated, `<pane>-<element>`) — the stable test selector. Tests query via `ElementQuery::match_predicate(|e| e.accessible_id() == ...)` until upstream adds a direct finder ([slint-ui/slint#9940](https://github.com/slint-ui/slint/pull/9940) shipped the property; finder remains TBD).

Slint propagates accessibility properties to descendants, so an `accessible-id` query returns one hit per descendant of the target widget — see `tisvlay-gui/tests/about_pane.rs` for `find_by_accessible_id` (returns the first match) and `count_by_id` (renders-or-not check).

### What we deliberately don't do

- **No pixel-snapshot tests.** Slint upstream ships none in `i-slint-backend-testing`, ecosystem research consistently flags pixel diffs as the most flake-prone test type, and the alternative (software renderer + bundled fonts + tolerance threshold) is heavy maintenance for low signal. Tabled in `docs/ideas.md` if a real need surfaces.
- **No MCP-driven CI tests.** Slint's MCP server is a dev-time introspection surface; `.claude/slint.md` documents the reliability quirks. Tests use the in-process testing backend exclusively.

### Pin

`i-slint-backend-testing` is pinned `=1.16.1` in both `tisvlay-gui` and `tisvlay-harness` Cargo.toml because the crate's README warns "preliminary API, no semver" and 1.17 already moved test helpers between modules ([slint-ui/slint#11516](https://github.com/slint-ui/slint/pull/11516) merged 2026-04-27).

## Test observability

### #[traced_test] — per-test log capture

Installs a subscriber that captures all `tracing` events. On failure, dumps them to stderr. On success, discards.

```rust
use tisvlay_test::traced_test;

#[traced_test]
#[tokio::test]
async fn my_test() {
    // ... test code ...
    assert!(logs_contain("expected log message"));
}
```

The `no-env-filter` feature is pinned so events from dependency crates (tisvlay-mock, tisvlay-test, the daemon) are captured — the upstream default only captures the test binary's own crate.

### tracing-mock — observability regression tests

Assert that production code emits specific spans/events with the right fields. Catches refactors that silently remove instrumentation.

```rust
use tisvlay_test::tracing_mock::{expect, subscriber};
use tracing::subscriber::set_default;

#[test]
fn publish_tv_emits_instrumented_debug_event() {
    let (sub, handle) = subscriber::mock()
        .new_span(expect::span().named("publish_tv").with_fields(
            expect::field("tv_id").with_value(&tracing::field::display("lr")),
        ))
        .enter(expect::span().named("publish_tv"))
        .event(expect::event().at_level(tracing::Level::DEBUG))
        .exit(expect::span().named("publish_tv"))
        .drop_span(expect::span().named("publish_tv"))
        .run_with_handle();

    let _guard = set_default(sub);
    // ... drive code that emits the span ...
    handle.assert_finished();
}
```

## Deterministic time

`tokio::time::pause()` + `advance()` enables tests to control the clock without real sleeps. Validated by the clock spike test (`tisvlay-test/tests/clock_spike.rs`).

Use `#[tokio::test(start_paused = true)]` — no explicit `pause()` needed.

**Caveat:** zbus uses internal timers that don't cooperate with Tokio's paused clock. Tests involving real D-Bus round-trips (e.g., session event tests) must use real-time sleeps. See [docs/todo.md](todo.md#session-event-tests-use-real-time-sleeps).

## TV ID rules

TV IDs must match `[A-Za-z0-9_]+` — validated at config load time. This ensures the ID can be used directly as a D-Bus object path component without lossy sanitization.

- `is_valid_tv_id(id)` — validation
- `tv_id_from_name(name)` — generation (lowercases, `_` for spaces, strips rest)
- Config load rejects invalid IDs with a suggested fix
- Duplicate IDs rejected at load time

Background: see [ruffsl/TisVlay#22](https://github.com/ruffsl/TisVlay/issues/22).

## Known constraints

### Name ownership is bus-global

`TestDaemon` and `TestSession` each register a well-known D-Bus name. Only one can exist per bus. Nextest's process-per-test model means this works today, but tests within a single binary that both create a `TestDaemon` would collide.

### Real-time sleeps in D-Bus tests

Session event tests use `sleep(50ms)` after injecting events. This is a pragmatic trade-off — deterministic time doesn't work with zbus. See [docs/todo.md](todo.md) for mitigation options (poll-with-timeout recommended for future improvement).

## Architecture decisions

These document **why** the framework is shaped this way:

1. **Private dbus-daemon per test binary** (not per test, not shared across binaries). Avoids zbus's `OnceCell` connection cache; amortizes 15-40ms spawn cost via `LazyLock`.

2. **`parking_lot::Mutex` for MockState** (not `tokio::sync::Mutex`, not `std::sync::Mutex`). Brief critical sections never held across `.await` — async mutex overhead unnecessary. `parking_lot` over `std` because it has no poison state, which eliminates `.expect("…poisoned")` boilerplate at every call site. Migration applies workspace-wide for short non-async critical sections.

3. **Rust-native fakes** (not Python dbusmock). Keeps toolchain Rust-only; zbus `ObjectServer` is expressive enough for the few services we mock.

4. **`from_connection` constructors** (not feature flags or doc(hidden)). General-purpose, useful outside tests. No dead code risk.

5. **ID → path allowed, path → ID never.** The D-Bus path is a derived artifact. `AddTv` returns the canonical ID; `DaemonClient::pair_tv` uses it directly.

6. **Handle wins over shorthand.** When both `MockHandle::simulate_user_*` and the config `pairing = "..."` shorthand are set, the handle takes precedence — it's the authoritative state machine input.

7. **`no-env-filter` on tracing-test.** Without it, events from dependency crates are invisible in test output. This is a non-obvious default that caused a test failure before being pinned.

## Future work

Deferred until the framework matures or a specific need arises:

- **Codecov upload** — pre-written in `.github/workflows/ci.yml`, commented out. Uncomment when the repo goes public (one-line change).
- **Mutation testing** (`cargo-mutants`) — worth running once coverage stabilizes; would surface tests that pass but don't actually assert meaningful properties.
- **Fuzzing** (`cargo-fuzz`) — needs clear input-parsing entry points; defer until something obvious emerges (e.g., SSAP message parsing).
- **Benchmark infrastructure** (`criterion`, `bencher`) — performance is not a current concern for tisvlay.
- **`cargo-deny` / `cargo-audit`** — supply chain security; separate concern from testing, tracked elsewhere.
