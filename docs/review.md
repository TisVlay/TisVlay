# Review: Testing & Measurement Framework (main..HEAD)

Review of 21 commits implementing Phases 1-6 of the testing framework plan: coverage
ratchet, nextest runner, insta snapshots, rstest parameterized tests, proptest property
tests, and the tisvlay-mock backend crate. Phase 7 (Codecov upload) deferred.

Build: clean. Tests: 94 pass.

---

## Z1. Phase 6 — `tisvlay-mock` crate is well-designed

**File:** [crates/tisvlay-mock/](crates/tisvlay-mock/)

The mock backend is more than a stub — it correctly models LG's behavior so the
daemon's real code paths (subscription forwarder, D-Bus property cache, reconnect
logic) all exercise against it:

- `MockConnection::mutate_and_push()` clones state under the lock and pushes to
  subscribers — same shape as LG's WebSocket update stream
- `Transport::Network` + `survives_source_ip_change() = false` matches LG, so
  carrier/route/NIC-failover paths run identically
- `subscribe_state()` sends an immediate snapshot then pushes on every mutation,
  matching SSAP subscribe semantics
- `MockConnection::power_off()` correctly closes the transport (`self.connected = false`)
  to mirror real backends
- `switch_input()` validates against the input list and returns a backend error,
  matching LG's behavior
- `start_pairing()` spawns a real `tokio::task` with `tokio::select!` between a 10ms
  timer and the command channel — exercises the same `PairingSession` machinery the
  real backend uses

**Severity:** Info (high-fidelity mock, well-aligned with the real backend)

---

## Z2. Mock feature gating is correct

**Files:** `crates/tisvlay-backends/Cargo.toml`, `crates/tisvlayd/Cargo.toml`

`tisvlay-backends` exposes a `mock` feature gated as `dep:tisvlay-mock`. `tisvlayd`
forwards via its own `mock` feature to `tisvlay-backends/mock`. Both are default-off,
so distro packages (`cargo build -p tisvlayd`) link zero mock code.

The integration test uses `#![cfg(feature = "mock")]` at the file level so it's
silently skipped when the feature is off — no test failures, no compile errors.

`create_registry()` registers the mock connector behind `#[cfg(feature = "mock")]`
without affecting LG registration. Clean composition.

**Severity:** Info (correct feature design)

---

## Z3. Phase 5 — proptest discovers edge cases at scale

**Files:** `crates/tisvlay-core/src/dbus_paths.rs`, `crates/tisvlay-core/src/wol.rs`

7 properties x 256 cases = ~1,800 randomized inputs per run. The properties are
well-chosen:

- `tv_path_suffix_is_valid` — invariant: only `[A-Za-z0-9_]`
- `tv_path_preserves_char_count` — invariant: char count never changes
- `tv_path_valid_input_unchanged` — passthrough for valid inputs
- `mac_parse_display_roundtrip` — `parse(mac.to_string()) == mac`
- `mac_parse_all_separator_styles` — colon/dash/plain equivalence
- `mac_parse_case_insensitive` — upper/lower equivalence
- `mac_parse_rejects_wrong_length` — failure mode property

Each uses `prop_assert*` (cooperates with shrinking) instead of `assert*`. Failing
seeds would be persisted to `.proptest-regressions` and replayed on future runs.

**Severity:** Info (correct property test design)

---

## Z4. Phase 4 — rstest converts 3 tests into 7 labelled cases

**File:** `crates/tisvlay-core/src/dbus_paths.rs`

The three flat `tv_path` tests are consolidated into one `#[rstest]` with 7 `#[case]`
entries. New cases (`with_dot`, `multiple_bad`, `numeric`, `all_alphanumeric`) cover
edge cases that weren't worth writing as separate functions before. Each shows up in
nextest with the case label, so failures point at the failing input.

**Severity:** Info (good demonstration of the pattern)

---

## Z5. Phase 3 — insta snapshots pin TvInfo wire format

**File:** `crates/tisvlay-core/tests/tv_info_snapshot.rs`

`TvInfo` is the externally-visible struct returned by `list_tvs()` and exposed as the
CLI's `--json` output. Snapshotting it as a stability contract is the right call —
any accidental field rename or reorder would break Nushell pipelines and downstream
scripts.

Three snapshots cover connected, disconnected, and list-of-two shapes. Reviewed via
`just test-snapshot-review`.

**Severity:** Info (correct application of snapshot testing)

---

## Z6. Phase 2 — nextest with CI/local profiles

**File:** `.config/nextest.toml`

Two profiles:
- `default` — local use, no retries, `slow-timeout = 60s` warning
- `ci` — `retries = 1` for transient flakes, `fail-fast = false` for full picture, JUnit XML output

Process-per-test isolation matters here because of D-Bus / netlink / file paths
global state. `just test-ci` uses the `ci` profile and produces JUnit XML for
`dorny/test-reporter` to publish inline.

**Severity:** Info (correct configuration for the workspace)

---

## Z7. Phase 1 — coverage ratchet with `cargo-llvm-cov`

**Files:** `justfile`, `.github/workflows/ci.yml`

Coverage measured at 19.59% baseline, 20.24% after the mock crate. The ratchet
(`just cov-ratchet`, `--fail-under-lines 19`) prevents regression without setting an
arbitrary high target. The `cov_baseline_lines` variable is in the justfile for easy
bumping on PRs that improve coverage.

The Phase 7 commit (8ee7656) consolidates coverage into the test step:
`NEXTEST_PROFILE=ci cargo llvm-cov --no-report nextest` runs tests once under
instrumentation, then `cargo llvm-cov report --lcov` and `--fail-under-lines` use
the cached profile data. One test run, both outputs.

**Severity:** Info (correct ratchet design)

---

## Z8. CI consolidation — Build step removed

**File:** `.github/workflows/ci.yml`

The Build step was redundant: `cargo llvm-cov nextest` produces a different cargo
fingerprint than `cargo build` (instrumented vs uninstrumented), so the Build step's
artifacts couldn't be reused by Test. `cargo clippy --all-targets` already does a
full uninstrumented compile (verifying the distro build), so it serves as the build
verification on its own.

The CI now has two compile stages:
- **Clippy** — uninstrumented build + lints (verifies distro compile)
- **Test** — instrumented build + nextest + lcov + ratchet

prek and `just check` still keep all four hooks for local incremental use. This is
the right tradeoff: CI optimizes for total time (no redundant compile), local
development optimizes for incremental feedback.

**Severity:** Info (correct optimization)

---

## Z9. justfile as single source of truth

**File:** `justfile`

21 recipes organized into `check`, `coverage`, `dev`, `package` groups via
`[group('...')]` attributes. `set shell := ["bash", "-euo", "pipefail", "-c"]` for
strict failure handling. The `cov-ratchet` recipe takes an optional `lines` parameter
defaulting to `cov_baseline_lines` so PRs can test higher targets without editing
the justfile.

prek hooks, VS Code tasks, and CI workflows all dispatch through `just`, so command
strings live in exactly one place. Renames and flag changes happen once.

The `package-build-musl` recipe is `[private]` (hidden from `just --list`) since
it's only a dependency of the `package-*` recipes.

**Severity:** Info (excellent task organization)

---

## Z10. `mutate_and_push` `.expect("...lock poisoned")` — acceptable for mock

**File:** `crates/tisvlay-mock/src/connection.rs`

The mock uses `std::sync::Mutex` and panics on poison via `.expect("mock state lock poisoned")`.
Each method that touches state has this same pattern. Lock poisoning only happens if
a previous holder panicked while holding the lock — for an in-memory mock with no
panicking code paths, this is essentially unreachable.

For a test mock, the current `.expect` is fine — a poisoned lock indicates a bug
in the mock itself, and panicking is the right response. Not actionable.

**Severity:** Info (acceptable for mock semantics)

---

## Z11. `MockConnection` async fns with no `.await`

**File:** `crates/tisvlay-mock/src/connection.rs`

`mutate_and_push` is sync (locks `std::sync::Mutex`, sends through `mpsc::Unbounded`)
so methods like `power_off`, `screen_on/off`, `set_volume` have no `.await`. This is
fine — they're forced to be `async` by the trait, and the no-op async overhead is
negligible.

**Severity:** Info (correct, just noting)

---

## Z12. Mock `start_pairing` always succeeds — Phase A.2 will add modes

**File:** `crates/tisvlay-mock/src/connector.rs`

```rust
Some(PairingCommand::SubmitInput(_)) => {
    // Mock doesn't use PIN input; succeed anyway.
    let _ = state_tx.send(PairingState::complete(...));
}
```

This is fine for current state but limits failure-mode pairing tests. The plan's
`MockPairingMode` enum will replace this with explicit Accept/Reject/Timeout/
DelayedAccept modes — captured as Phase A.2.

**Severity:** Info (acknowledged limitation, planned for Phase A)

---

## Z13. Mock crate has no `#[cfg(test)]` unit tests

**File:** `crates/tisvlay-mock/src/`

The mock crate's test coverage comes entirely from
`crates/tisvlay-backends/tests/mock_backend.rs` (6 integration tests). The mock crate
itself has no `mod tests` — every method is exercised through the integration tests.

This is acceptable for a small implementation crate, but the integration tests live
in a different crate, so coverage tools may not credit them correctly. If
`crates/tisvlay-mock/` shows as 0% covered in the report, that's why — the call
sites are in `tisvlay-backends/tests/`.

**Fix direction (low priority):** Verify that llvm-cov credits the mock crate from
cross-crate test calls. If not, add a brief `mod tests` in
`crates/tisvlay-mock/src/connection.rs` to verify state mutation locally.

**Severity:** Info (worth verifying coverage attribution)

---

## Z14. `subscription_tx` is single-subscriber — matches LG

**File:** `crates/tisvlay-mock/src/connection.rs`

`MockConnection::subscription_tx: Option<mpsc::UnboundedSender<TvState>>` only
supports one subscriber at a time — calling `subscribe_state()` twice replaces the
previous sender. Real backends (LG) also support only one active subscription channel
since the WebSocket reader has one mpsc tx, so this matches reality.

If multi-subscriber support is ever needed for testing scenarios,
`tokio::sync::broadcast` would be the right swap.

**Severity:** Info (matches real backend behavior)

---

## Z15. `--all-features` correctly includes mock

**File:** `justfile`

```just
test:
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc
```

`--all-features` is correct — it includes the `mock` feature so `mock_backend.rs`
runs. Without it, the mock integration tests would be silently skipped. The doc
tests run separately because nextest doesn't support them. Both invocations include
`--all-features`, which is consistent.

**Severity:** Info (correct invocation)

---

## Z16. `just check` vs `just test-ci` — correct local/CI split

**File:** `justfile`

`just check` runs `fmt build clippy test` for incremental local development. CI uses
`just clippy` + `just test-ci` instead, which is more efficient (no separate Build
step). This asymmetry is documented in the test-ci recipe comment.

**Severity:** Info (correct local/CI split)

---

## Z17. No documented way to run the daemon with mock feature

**Files:** `justfile`, `.claude/CLAUDE.md`

The `mock` feature exists and forwards correctly, but there's no documentation in
the daemon's `daemon-start` recipe or CLAUDE.md about how to enable it. The plan
mentions `cargo run --bin tisvlayd --features mock` but it's not in the `dev` recipes.

The justfile's `daemon-start` recipe doesn't include `--features mock`, so contributors
who want to test against mock TVs would need to override it manually.

**Fix direction:** Add a `daemon-start-mock` recipe to the justfile, or document
`cargo run --features mock` in the dev section of CLAUDE.md.

**Severity:** Low (DX gap, not a correctness issue)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| Z1 | **Info** | Mock crate has high fidelity to real backend behavior |
| Z2 | **Info** | Feature gating is correct, distro builds get zero mock code |
| Z3 | **Info** | Proptest properties are well-chosen and use prop_assert |
| Z4 | **Info** | rstest demonstrates the pattern with new edge cases |
| Z5 | **Info** | insta snapshots correctly pin the TvInfo wire format |
| Z6 | **Info** | nextest CI/local profiles configured correctly |
| Z7 | **Info** | Coverage ratchet is correctly configured |
| Z8 | **Info** | Build step removed, clippy serves as build verification |
| Z9 | **Info** | justfile is excellent task organization |
| Z10 | **Info** | `.expect` on mutex poisoning is acceptable for mock |
| Z11 | **Info** | async fns with no .await are forced by trait |
| Z12 | **Info** | Pairing always succeeds — planned for Phase A.2 |
| Z13 | **Info** | Mock crate has no unit tests (covered by integration tests) |
| Z14 | **Info** | Single subscriber matches real backend |
| Z15 | **Info** | `--all-features` correctly includes mock |
| Z16 | **Info** | check vs test-ci split is correct |
| Z17 | **Low** | No documented way to run daemon with mock feature for dev |

## Overall Assessment

This branch implements Phases 1-6 of the testing plan thoroughly. The most impactful
addition is the **mock backend** — it's not a stub, it's a high-fidelity LG emulator
with the same transport semantics, push subscription model, and `survives_source_ip_change`
behavior. This means future Phase B tests can exercise the daemon's real code paths
(state forwarder, D-Bus property cache, NIC failover) without hardware.

The coverage ratchet, nextest setup, and CI consolidation form a cohesive testing
infrastructure. The justfile as single source of truth is the right pattern — prek
hooks, VS Code tasks, and CI all dispatch through it.

One low-severity item: **Z17** — no documented way to run the daemon with the mock
feature enabled for interactive development. Adding a `daemon-start-mock` recipe or
documenting `cargo run --features mock` in CLAUDE.md would close this DX gap.

No actionable findings beyond Z17. Phase A (discovery + pairing modes) and Phase B
(daemon integration tests) are the natural follow-ups, and the mock crate's design
supports both cleanly.

## Idiomatic Rust observations (no actionable items)

A few patterns I considered for refactoring but ruled out:

- **`pub struct MockBackend;` + `new()`** could become `#[derive(Default)]` with no
  `new()`, but losing the explicit constructor makes the API less discoverable. Style
  preference; current form is fine.

- **`MockConnector::start_pairing` spawned task** uses `tokio::select!` between a
  10ms timer and the command channel. Could use `tokio::time::timeout` instead, but
  the current form is more flexible if more branches are added later (Phase A.2).

- **`MockConnection::mutate_and_push`** closure-based mutation is the cleanest way
  to release the lock before sending through the channel. The clone-then-send pattern
  is correct.

- **`parse_config`** in connector.rs duplicates the pattern from the LG backend.
  Could be extracted into a shared helper, but with only two backends and slightly
  different error contexts, the duplication is acceptable.

None of these are worth the refactoring cost for the current state.
