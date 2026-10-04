# Switch CI (or workspace) to nightly Rust for build-time knobs

The build-system design ([`docs/build.md`](../build.md)) leaves two candidates on the table because the workspace pins to stable Rust:

- **`-Zthreads=N` parallel rustc frontend** — flagged as 10–30% cold-build win in the SOTA survey; nightly-only as of rustc 1.95.
- **`-Zcodegen-backend=cranelift`** — 10–20% codegen-time reduction; nightly-only.

Greenfield, pre-release projects can absorb nightly's risk more cheaply than published crates, so this is worth keeping on the table even though we said no during the sprint.

## What this would buy

Honest expected gain on this workspace, with the share-target restructure ([`docs/build.md`](../build.md) "Build pipeline") already in place:

| Knob | Local (32-core) | CI (2-core ubuntu-latest) | Compatibility |
|---|---|---|---|
| `-Zthreads=8` | ~0 (already critical-path-bound) | **10–20% cold wall** — CI's collapsed parallelism is exactly the constraint this fixes | Works under `-C instrument-coverage` |
| `-Zcodegen-backend=cranelift` | ~10–20% on dev/test codegen | similar | **Incompatible with `-C instrument-coverage`** (instrumentation requires LLVM). Only useful on non-instrumented paths — and our share-target sprint collapses everything onto the instrumented path, so cranelift has nothing left to help. |

So the *real* win behind switching is `-Zthreads`. Cranelift is mostly orphaned by the share-target restructure.

## What this would *not* buy

- **Solving the rustflags-differ artifact-sharing problem.** Nothing on nightly today addresses it. `-Zfine-grain-locking` (Jan 2026, nightly) and `CARGO_BUILD_DIR_LAYOUT_V2` (opt-in) are about concurrent invocations and layout reorganization, not artifact reuse across `RUSTFLAGS`. The 2026 cross-workspace-cache goal might eventually generalize to it; not on nightly yet.
- **Stable-toolchain story for downstream consumers.** If we ever publish, downstream users on stable get the same source but their CI doesn't see the nightly wins. Manageable, but a property to give up.

## Trade-offs to budget for

- **Nightly clippy adds new lints periodically.** Each new lint that fires on our code is a small CI breakage to fix.
- **Toolchain pinning strategy matters.** `channel = "nightly"` (always-latest) gets max breakage exposure; `channel = "nightly-YYYY-MM-DD"` is safer but needs occasional bumps.
- **rust-toolchain.toml is checked in.** Workspace-wide change unless we add a CI-only override (split-brain complexity, lints fire differently locally vs CI).
- **A few workspace deps assume stable.** Usually fine; occasionally a transient regression.

## Trigger conditions to re-open

Pick this up when **any** of these fires:

- CI cycle time on `just test-ci` becomes a real bottleneck for PR turnaround (i.e., the share-target restructure didn't trim enough on its own).
- The 2026 cross-workspace-cache nightly preview lands ([cargo#5931](https://github.com/rust-lang/cargo/issues/5931)) — being on nightly already means we can opt in for free.
- A nightly stabilization roadmap for `-Zthreads` materializes — at that point, switching pre-stabilization-on-stable buys little extra.

## Cleanish paths

If we re-open this, the two viable shapes are:

1. **Move workspace to nightly** (single `rust-toolchain.toml` change). Folds `-Zthreads` into both local and CI builds. Simple but workspace-wide.
2. **CI-only nightly via workflow override.** Keeps local dev on stable; adds split-brain complexity (different lint surface locally vs CI). Needs its own plan doc to land safely.

Path 1 is simpler and aligns with the project's greenfield posture; path 2 is reasonable only if local-dev stability under stable Rust matters more than the orthogonal complexity.

## Out of scope

- `cargo-careful` and `miri` (nightly tools) and `cargo-mutants` (see [mutation-testing.md](mutation-testing.md)) — orthogonal to build time, not relevant here.
- Build-time wins that are already captured by other sprints (rust-lld default, `Swatinem/rust-cache`).
