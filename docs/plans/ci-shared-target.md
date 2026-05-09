# CI shared target sprint

> **Status:** drafted 2026-05-09, not started. Restructure `just clippy` + `just test-ci` so they share `target/llvm-cov-target/` instead of compiling the workspace twice with different rustflags. Centralize the `mock`-only feature subset so it can't drift across recipes. Track each step in the table at the bottom.

Continuation of [`build-time-optimization.md`](build-time-optimization.md). That sprint shipped knob 1 (profile tweaks, -12% cold), knob 3 (`cache-on-failure`), knob 4 (machete cleanup), and knob 10 (Wayland-only GUI, -90 s CPU per cold). The remaining duplication is the **double workspace compile** in CI: `just clippy` builds the entire workspace under `--profile test --all-features` to populate `target/debug/`, then `just test-ci` rebuilds it under `RUSTFLAGS=-C instrument-coverage --features mock,mock` into `target/llvm-cov-target/`. Cargo can't share artifacts because rustflags are part of the fingerprint — this is by design today.

Upstream is actively moving on adjacent work, none of which solves our exact problem on stable today. The [2025h2 build-dir-layout rework](https://rust-lang.github.io/rust-project-goals/2025h2/cargo-build-dir-layout.html) ([issue 401](https://github.com/rust-lang/rust-project-goals/issues/401)) split `target/` into `artifact-dir` + `build-dir` and shipped the split itself in Rust 1.91 (we're on 1.95). Per the goal author's [March 2026 status update](https://github.com/rust-lang/rust-project-goals/issues/401#issuecomment-2716968253), nightly now also exposes `-Zfine-grain-locking` and a separated `.cargo-lock` so `cargo check` and `cargo build` can run in parallel on the same target — but those address *concurrent cargo invocations*, not artifact reuse across different rustflags. Stabilization of the rework "within 2026" was the author's stated goal in March. The follow-up [2026 cross-workspace-cache goal](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html) ([issue 626](https://github.com/rust-lang/rust-project-goals/issues/626), tracked in [cargo#5931](https://github.com/rust-lang/cargo/issues/5931)) is the design+nightly phase; the goal doc lists "share build artifacts between profiles (e.g. debug, release)" as a target feature, which is the closest framing to our rustflags-differ pain — but cache-key design isn't committed yet, so whether it generalizes to "instrumented vs not" is open.

Realistic timeline for a stable upstream solution that obsoletes this sprint's workaround: **2027 at the earliest** (build-dir layout stabilization in 2026, then cross-workspace-cache design → nightly → stabilization). Worth landing the `show-env` workaround now; it pays back every CI run for the years until then. Revisit this plan when (a) the cross-workspace-cache nightly preview lands and we can see if its cache key handles different rustflags, or (b) ranger-ross's continued build-dir locking work surfaces a "share artifacts across rustflags" feature directly.

## Approach: option (A) — instrumented single build

Run **clippy + nextest + doc-test** all under the same instrumented env via `cargo llvm-cov show-env --export-prefix`. Single target dir. Single feature set. Estimated wall savings ~3–4 min cold CI; estimated disk savings ~4 GB.

Pseudo-code for the new `test-ci` recipe shape:

```
test-ci:
    eval "$(cargo llvm-cov show-env --export-prefix)"
    cargo clippy --workspace --all-targets \
        --features {{ ci_features }} -- -D warnings
    cargo llvm-cov --no-report nextest --workspace \
        --features {{ ci_features }}
    cargo test --workspace --features {{ ci_features }} --doc
    cargo llvm-cov report --lcov ...
    cargo llvm-cov report --fail-under-lines {{ cov_baseline_lines }} ...
```

The CI workflow then collapses to a single `just test-ci` (no separate `just clippy` step).

## Trade-offs we accept

For our current source, none of these change behavior. They are guardrails that move from CI to local prek.

| Aspect | Today | After |
|---|---|---|
| Clippy features | `--all-features` (CI) | `{{ ci_features }}` (CI); `--all-features` still in prek locally |
| `--all-features` compile gate in CI | yes (via clippy) | no for source code, but `dev-mcp` adds zero local source today (audit: every `#[cfg(feature = "X")]` in `crates/*/src/` is `mock`, `discovery`, `dbus`, or `logging`, all of which `{{ ci_features }}` reaches transitively) |
| Clippy wall time | normal | ~10–20% slower per crate (estimated, not yet benchmarked on our workspace) |
| Total target/ disk | ~12 GB | ~7–8 GB |
| Cold CI wall | 7–10 min | 4–6 min (estimated) |

## Centralize the feature subset

Lift the literal `tisvlay-backends/mock,tisvlayd/mock` into a `ci_features` justfile variable so the four call sites (clippy, llvm-cov nextest, doc test, `_cov-collect`) can't drift. Comment block on the variable carries the rationale + trigger condition:

> When to extend `ci_features`:
> - New `#[cfg(feature = "X")]` block in `src/` where `X` isn't already reached by `mock` / `discovery` / `dbus` / `logging` → add the feature here, or CI silently stops linting and testing it.
> - New workspace crate with a mock-style test-only feature → add `<crate>/mock` here.
> - `dev-mcp` becomes load-bearing for tested code (today it's dep-only) → either add `tisvlay-gui/dev-mcp` here, or accept that prek-only is the gate.

## Documentation surface

| File | What goes there |
|---|---|
| `justfile` (block on `ci_features`) | Canonical rationale + extension trigger. Load-bearing — first thing anyone touching a CI cargo flag sees. |
| `docs/plans/ci-shared-target.md` (this file) | Sprint history, decision rationale, links. Archive when the recipe stabilizes. |
| `docs/testing.md` (new short paragraph) | One sentence + link: "CI uses a mock-feature subset; see justfile `ci_features` for the canonical list." |
| `CLAUDE.md` "Linting & CI" section | One-line pointer to the justfile variable so future sessions don't rediscover it. |

## Steps

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `ci_features := "..."` justfile variable + comment block | todo | — |
| 2 | Switch existing `_cov-collect` and `test-ci` cargo invocations to `{{ ci_features }}` (still using current pipeline shape) | todo | Pure refactor; no CI behavior change yet. Should pass `just test-ci` locally. |
| 3 | Refactor `test-ci` to the `cargo llvm-cov show-env` single-build shape (clippy + nextest + doc-test under instrumentation) | todo | The actual win. |
| 4 | Update `.github/workflows/ci.yml` — drop the separate `Clippy` step; `Test` step alone now covers both | todo | Watch CI run for the wall-time delta. |
| 5 | Update `docs/testing.md` and `CLAUDE.md` with pointers | todo | One-line each. |
| 6 | Measure: cold CI wall before vs after; record in this plan's discussion log | todo | Compare against the most recent green CI run on `master`. |

## Risks / unknowns to watch

- **Clippy under `-C instrument-coverage`** — no public benchmark for the slowdown on our workspace. If it's > 30% the wall savings narrow.
- **Clippy lint output under instrumentation** — should be identical (instrumentation is a codegen pass; clippy runs at HIR level). No known reports of mismatch, but worth eyeballing the first CI run.
- **Disk watermark mid-run** — `cargo clippy` populates `target/llvm-cov-target/` partially before nextest starts. If the partial population peaks bigger than full instrumented build, we could regress disk pressure. Most likely fine but measurable.
- **`show-env --export-prefix` shell-quoting** — `eval` of its output across `bash -euo pipefail` (justfile shell) needs verification. May need `set -a` or quoting tweaks.

## Discussion log

Append findings as we go. Keep terse — full data lives in CI logs.

- **2026-05-09** — plan drafted. No steps started yet.

## Out of scope

- Splitting CI into parallel jobs (option D in the original survey). Real projects (tokio, ripgrep) do this, but it duplicates Swatinem cache and fights for the 10 GB cache budget. Reconsider if PR cycle time becomes pressing or if the workspace grows past what one job can hold.
- `sccache` with remote backend. Setup overhead vs. payoff is poor for a single ubuntu-latest job.
- `cargo-hack` feature-graph testing. Useful in principle but each features-set materializes its own target dir; would compound the disk problem we're trying to solve.

## References

- [taiki-e/cargo-llvm-cov `show-env`](https://github.com/taiki-e/cargo-llvm-cov?tab=readme-ov-file#get-coverage-of-cc-code-linked-to-rust-libtest-binary) — the `--export-prefix` flag is the contract this plan rests on.
- [cargo issue #8716](https://github.com/rust-lang/cargo/issues/8716) — RUSTFLAGS in fingerprint, the upstream constraint that forces this restructure.
- Upstream cargo work on the broader artifact-sharing problem (status of these affects when this sprint's workaround can retire):
  - [Rework Cargo Build Dir Layout (issue 401, 2025h2, closed)](https://github.com/rust-lang/rust-project-goals/issues/401) — read the comment thread for the actual shipping cadence.
  - [Cargo cross workspace cache (issue 626, 2026, open)](https://github.com/rust-lang/rust-project-goals/issues/626).
  - [2026 goal doc](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html).
  - [cargo#5931 — implementation tracking issue](https://github.com/rust-lang/cargo/issues/5931).
- Sibling sprint: [`build-time-optimization.md`](build-time-optimization.md).
