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
- **Poll for the expected state with a timeout** instead of a fixed
  sleep (recommended). Polls at short intervals, succeeds as soon as
  the condition is met, retries up to a timeout under CI load:

  ```rust
  async fn wait_for(f: impl Fn() -> bool, timeout: Duration) {
      let deadline = tokio::time::Instant::now() + timeout;
      while !f() && tokio::time::Instant::now() < deadline {
          tokio::time::sleep(Duration::from_millis(5)).await;
      }
      assert!(f(), "condition not met within {:?}", timeout);
  }
  ```

  This makes tests both faster (succeeds in ~5ms instead of sleeping
  a fixed 50ms) and more resilient (retries up to the deadline under
  CI load instead of failing on first miss).

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

- Document the constraint (done — in `tisvlay-test/src/lib.rs`)
- Use nextest's `test-groups` to explicitly serialize bus-dependent
  tests within a binary if needed
- Generate unique service names per test (e.g., `org.tisvlay.Daemon.{uuid}`)
  and parameterize all proxies — high complexity, low value until the
  constraint actually bites

---

## CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround

**Priority**: low
**Found by**: CI build-time work, 2026-05

### Context

`just test-ci` collapses clippy + nextest + doc tests + coverage report
into a single coverage-instrumented build via `eval "$(cargo llvm-cov
show-env --sh)"`. The workaround exists because cargo's fingerprint
includes `RUSTFLAGS`, so two cargo invocations with different rustflags
(one bare, one `-C instrument-coverage`) cannot share artifacts. See
[build.md](build.md) "Build pipeline" for the full design.

The upstream goal that may obsolete this is the [Cargo cross-workspace-cache
2026 effort](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html)
([issue 626](https://github.com/rust-lang/rust-project-goals/issues/626);
implementation [cargo#5931](https://github.com/rust-lang/cargo/issues/5931)).
The 2026 goal doc explicitly lists "share build artifacts between profiles"
as a target feature — the closest framing to our rustflags-differ pain — but
cache-key design isn't committed yet, so whether it generalizes to
"instrumented vs not" is open.

### Risk

Low. The workaround is stable on cargo + rustc 1.95 and the `show-env`
contract has held since cargo-llvm-cov 0.6. If cargo-llvm-cov ever
drops `show-env`, the test-ci recipe would need to fall back to two
separate cargo invocations.

### Mitigation options / re-evaluation triggers

- When the cross-workspace-cache nightly preview lands, evaluate
  whether its cache key handles different rustflags. If yes, the
  show-env workaround becomes deletable.
- Any further `cargo-llvm-cov` upstream API change would surface in
  the `just test-ci` recipe — easy to track.

### See also

- [Stray `.profraw` files outside `target/`](concerns/profraw-stray-files-outside-target.md) — sibling concern, same root cause (cargo's RUSTFLAGS-fingerprint coupling) but different surface (runtime artifacts produced by lingering instrumented test binaries vs. pipeline-rebuild cost).

---

## Stray `.profraw` files outside `target/` from instrumented binaries

**Priority**: low
**Found by**: post-merge audit of working-tree artifacts, 2026-05

### Context

LLVM's coverage runtime (`compiler-rt`'s profile library, linked into binaries built with `-C instrument-coverage`) writes a `default_<hash>_<pid>.profraw` to the test binary's *current working directory* whenever `LLVM_PROFILE_FILE` is unset. Under nextest that cwd is the per-crate directory, so stray artifacts accumulate in `crates/*/` — outside `target/`, surviving `cargo clean` / `rm -rf target/`, and showing up as untracked files in `git status`.

The leak path: a past `cargo llvm-cov` (or manual `RUSTFLAGS=...` invocation) that didn't isolate to `target/llvm-cov-target/` left instrumented test binaries lingering in `target/debug/deps/`. Cargo's fingerprinting eventually built fresh uninstrumented siblings but didn't garbage-collect the old instrumented ones. Anything that runs them outside the cargo-llvm-cov flow (plain `cargo test`, IDE test actions, doctest harnesses, CI steps that bypass the recipe) leaks profraw to cwd.

The workspace's `.cargo/config.toml` `[env]` block defaults `LLVM_PROFILE_FILE` to `target/profraw/default_%m_%p.profraw` (`relative = true`). Cargo `[env]` only applies when the var isn't already in the calling shell, so coverage-tool paths that export their own `LLVM_PROFILE_FILE` (e.g., `eval "$(cargo llvm-cov show-env --sh)"`) shadow this default — no conflict with the actual coverage pipeline. Every other test path inherits the default and writes under `target/`, restoring the invariant that `rm -rf target/` removes all build-time outputs.

See [concerns/profraw-stray-files-outside-target.md](concerns/profraw-stray-files-outside-target.md) for the full layering analysis, upstream issue survey, comparison with cargo-tarpaulin, and re-evaluation triggers.

### See also

- [CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround](#ci-test-ci-pipeline-depends-on-cargo-llvm-cov-show-env-workaround) — sibling concern, same root cause.
