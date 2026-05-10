## CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround

**Priority**: low
**Found by**: CI build-time work, 2026-05

### Context

`just test-ci` collapses clippy + nextest + doc tests + coverage report
into a single coverage-instrumented build via `eval "$(cargo llvm-cov
show-env --sh)"`. The workaround exists because cargo's fingerprint
includes `RUSTFLAGS`, so two cargo invocations with different rustflags
(one bare, one `-C instrument-coverage`) cannot share artifacts. See
[build.md](../build.md) "Build pipeline" for the full design.

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

- [Stray `.profraw` files outside `target/`](profraw-stray-files-outside-target.md) — sibling concern, same root cause (cargo's RUSTFLAGS-fingerprint coupling) but different surface (runtime artifacts produced by lingering instrumented test binaries vs. pipeline-rebuild cost).
