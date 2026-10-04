## CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround

**Priority**: low
**Found by**: CI build-time work, 2026-05

### Context

`just test-ci` collapses clippy + nextest + doc tests + coverage report into a single coverage-instrumented build via `eval "$(cargo llvm-cov show-env --sh)"`. The workaround exists because an instrumented build and a plain one can't share artifacts, so running them as separate cargo invocations compiles the workspace twice. See [build.md](../build.md) "Build pipeline" for the full design.

show-env routes every later cargo command through cargo-llvm-cov's `RUSTC_WRAPPER`, and it doesn't choose a target directory. cargo doesn't rebuild when only `RUSTC_WRAPPER` changes, so the recipe pins `CARGO_TARGET_DIR=target/llvm-cov-target/` itself; without the pin, `test-ci` reuses an uninstrumented `just build` and the report fails for want of `.profraw` files.

The upstream goal that may obsolete this is the [Cargo cross-workspace-cache 2026 effort](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html) ([issue 626](https://github.com/rust-lang/rust-project-goals/issues/626); implementation [cargo#5931](https://github.com/rust-lang/cargo/issues/5931)). The 2026 goal doc explicitly lists "share build artifacts between profiles" as a target feature — the closest framing to our instrumented-vs-plain pain — but cache-key design isn't committed yet, so whether it generalizes to "instrumented vs not" is open.

### Risk

Low, but not zero: the recipe depends on how show-env routes the build, and that has changed under it before (the switch to `RUSTC_WRAPPER` broke `test-ci` until the target dir was pinned). The symptom is unmistakable: a coverage report that fails with no profile data. If cargo-llvm-cov ever drops `show-env`, the recipe would need to fall back to two separate cargo invocations.

### Mitigation options / re-evaluation triggers

- When the cross-workspace-cache nightly preview lands, evaluate whether its cache key handles instrumented vs plain builds. If yes, the show-env workaround becomes deletable.
- A cargo-llvm-cov bump: run `just test-ci` once and confirm the report finds profile data.

### See also

- [Stray `.profraw` files outside `target/`](profraw-stray-files-outside-target.md) — sibling concern, same root cause (instrumented builds can't share artifacts with plain ones) but different surface (runtime artifacts produced by lingering instrumented test binaries vs. pipeline-rebuild cost).
