# CI shared target sprint

> **Status:** sprint complete 2026-05-09. Cumulative warm-CI saving on the Test+Clippy phase: **16m 45s → 3m 15s (-81%)**. Two wins: (1) the `show-env` single-build collapses what were two cargo invocations with mismatched rustflags into one shared target dir; (2) dropping `cargo-about` from the CI toolchain install lets the build script fall back to a stub blob, skipping a 3-minute `cargo-about generate --workspace --all-features` walk on every cold build. Steps + measurements in the table below.
>
> | Step | Status |
> |---|---|
> | 1 — `ci_features` justfile var + comment | ✅ shipped |
> | 2 — switch existing recipes to `{{ ci_features }}` | ✅ shipped |
> | 3 — `cargo llvm-cov show-env` single-build refactor | ✅ shipped |
> | 4 — drop separate Clippy step from `ci.yml` | ✅ shipped |
> | 5 — doc pointers in `docs/testing.md` + `CLAUDE.md` | ✅ shipped |
> | 6 — measure CI delta vs main | ✅ -81% warm; -3 min from cargo-about drop bonus |

Continuation of [`build-time-optimization.md`](build-time-optimization.md). That sprint shipped knob 1 (profile tweaks, -12% cold), knob 3 (`cache-on-failure`), knob 4 (machete cleanup), and knob 10 (Wayland-only GUI, -90 s CPU per cold). The remaining duplication is the **double workspace compile** in CI: `just clippy` builds the entire workspace under `--profile test --all-features` to populate `target/debug/`, then `just test-ci` rebuilds it under `RUSTFLAGS=-C instrument-coverage --features mock,mock` into `target/llvm-cov-target/`. Cargo can't share artifacts because rustflags are part of the fingerprint — this is by design today.

Upstream is actively moving on adjacent work, none of which solves our exact problem on stable today. The [2025h2 build-dir-layout rework](https://rust-lang.github.io/rust-project-goals/2025h2/cargo-build-dir-layout.html) ([issue 401](https://github.com/rust-lang/rust-project-goals/issues/401)) split `target/` into `artifact-dir` + `build-dir` and shipped the split itself in Rust 1.91 (we're on 1.95). Per the goal author's [March 2026 status update](https://github.com/rust-lang/rust-project-goals/issues/401#issuecomment-2716968253), nightly now also exposes `-Zfine-grain-locking` and a separated `.cargo-lock` so `cargo check` and `cargo build` can run in parallel on the same target — but those address *concurrent cargo invocations*, not artifact reuse across different rustflags. Stabilization of the rework "within 2026" was the author's stated goal in March. The follow-up [2026 cross-workspace-cache goal](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html) ([issue 626](https://github.com/rust-lang/rust-project-goals/issues/626), tracked in [cargo#5931](https://github.com/rust-lang/cargo/issues/5931)) is the design+nightly phase; the goal doc lists "share build artifacts between profiles (e.g. debug, release)" as a target feature, which is the closest framing to our rustflags-differ pain — but cache-key design isn't committed yet, so whether it generalizes to "instrumented vs not" is open.

Realistic timeline for a stable upstream solution that obsoletes this sprint's workaround: **2027 at the earliest** (build-dir layout stabilization in 2026, then cross-workspace-cache design → nightly → stabilization). Worth landing the `show-env` workaround now; it pays back every CI run for the years until then. Revisit this plan when (a) the cross-workspace-cache nightly preview lands and we can see if its cache key handles different rustflags, or (b) ranger-ross's continued build-dir locking work surfaces a "share artifacts across rustflags" feature directly.

## Approach: option (A) — instrumented single build

Run **clippy + nextest + doc-test** all under the same instrumented env via `cargo llvm-cov show-env --sh`. Single target dir. Single feature set. Estimated wall savings ~3–4 min cold CI; estimated disk savings ~4 GB.

Pseudo-code for the new `test-ci` recipe shape:

```
test-ci:
    eval "$(cargo llvm-cov show-env --sh)"
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
| 1 | Add `ci_features := "..."` justfile variable + comment block | done | Defined alongside test-ci recipe; comment block carries rationale + extension triggers. |
| 2 | Switch existing `_cov-collect` and `test-ci` cargo invocations to `{{ ci_features }}` (still using current pipeline shape) | done | All four call sites updated. Also fixed `_cov-collect`'s doc-test step which still used `--all-features` (inconsistent with the test-ci fix from the previous sprint). Verified: `just test-ci` passes locally, coverage 52.48% unchanged. |
| 3 | Refactor `test-ci` to the `cargo llvm-cov show-env` single-build shape (clippy + nextest + doc-test under instrumentation) | done | Multi-line bash recipe under `#!/usr/bin/env bash`; `eval "$(cargo llvm-cov show-env --export-prefix)"` exports RUSTFLAGS + CARGO_TARGET_DIR; clippy / nextest / doc-test all share `target/llvm-cov-target/`. Cold local wall: **97 s** (single-target single-pass). Coverage 52.27% (was 52.48%; -0.21 pp, well above 52 ratchet). 328/328 tests still run. |
| 4 | Update `.github/workflows/ci.yml` — drop the separate `Clippy` step; `Test` step alone now covers both | done | Single `Test` step running `just test-ci` now covers everything. Comment in the workflow points back at the recipe + plan. |
| 5 | Update `docs/testing.md` and `CLAUDE.md` with pointers | done | Short paragraph in `docs/testing.md` `## Coverage` section and a comment in `CLAUDE.md`'s `## Linting & CI`. Both link back to this plan and to `ci_features`. |
| 6 | Measure: cold CI wall before vs after; record in this plan's discussion log | done | Cumulative warm-CI Test+Clippy phase wall: 16m 45s (main) → 6m 27s (share-target) → 3m 15s (+ cargo-about drop). Numbers in the discussion log below. |

## Risks / unknowns to watch

- **Clippy under `-C instrument-coverage`** — no public benchmark for the slowdown on our workspace. If it's > 30% the wall savings narrow.
- **Clippy lint output under instrumentation** — should be identical (instrumentation is a codegen pass; clippy runs at HIR level). No known reports of mismatch, but worth eyeballing the first CI run.
- **Disk watermark mid-run** — `cargo clippy` populates `target/llvm-cov-target/` partially before nextest starts. If the partial population peaks bigger than full instrumented build, we could regress disk pressure. Most likely fine but measurable.
- **`show-env --sh` shell-quoting** — `eval` of its output across `bash -euo pipefail` (justfile shell) needs verification. May need `set -a` or quoting tweaks.

## Findings

### The remaining `tisvlay-gui` dual-compile is from `cargo clippy` vs `cargo nextest`, not from `cfg(test)`

A clean cold `just test-ci` after the show-env work shows two `Compiling tisvlay-gui` lines, while every other workspace crate's `Compiling X` count is one. The initial hypothesis was the lib's `cfg(test)` blocks producing a separate test-mode lib variant; an experiment moving the four `cfg(test)` modules from `src/` to `tests/` integration-test files and adding `[lib] test = false` to the manifest **did not** eliminate the second compile. Wall time was unchanged (~98 s cold) and `cargo-llvm-cov` coverage *dropped* from 52.27% → 50.34% because the report excludes `tests/*.rs` files, so the ~800 lines of test code that previously counted (when sitting inside `cfg(test) mod tests` blocks compiled into the lib) stopped counting toward both numerator and denominator. The experiment was reverted.

The real cause, confirmed by `cargo clippy ... -v` showing rustc invocations: **`cargo clippy` and `cargo nextest run` are different cargo "modes" that emit different artifact kinds**. clippy emits `--emit=dep-info,metadata` (rmeta only — interface, no machine code); nextest emits `--emit=dep-info,metadata,link` (rmeta + `.rlib`). cargo's fingerprint distinguishes the two artifact kinds, so the rmeta clippy produced cannot satisfy the link nextest needs, and **every workspace crate is recompiled in the second cargo invocation**.

This applies to all 11 workspace crates, not just `tisvlay-gui`. The reason `tisvlay-gui` *looks* uniquely dual-compiled when you grep for "Compiling X v0.1.0" is purely cosmetic:

| Crate | clippy step verb | nextest step verb |
|---|---|---|
| `tisvlay-core`, `-test`, `-lg`, `-mock`, `-session`, `-cli`, `-tray`, `-backends`, `-harness`, `tisvlayd` | **Checking** (the rmeta-only emit) | **Compiling** (the link emit) |
| `tisvlay-gui` | **Compiling** | **Compiling** |

`tisvlay-gui` shows "Compiling" in *both* steps because it is the only workspace crate with a `build.rs` (slint-build's `compile()` produces source via `OUT_DIR` that `slint::include_modules!()` `include!()`s). The build script must be compiled to a *binary* and *run* before clippy can check the lib — and cargo prints "Compiling" once per crate whenever any unit needs link-mode compilation, which the build script binary always does. Other workspace crates have no build script, so clippy's first touch of them is a lib rmeta-check, which cargo labels "Checking".

Net: there's nothing special about `tisvlay-gui`'s dual-compile. The whole workspace is dual-compiled by clippy → nextest. We just notice tisvlay-gui because the verbs collide.

What this means for the related earlier work:

- Knob 6 of `build-time-optimization.md` (the cfg(test) refactor) was dropped citing "parallelism on 32-core hides this". The deeper reason it would not have helped on CI either: even with cfg(test) gone and `[lib] test = false`, the clippy-vs-build cache-kind mismatch keeps the second compile around. Knob 6's dropped status is correct; the rationale captured here supersedes the earlier "parallelism" framing.
- Step 3's discussion-log line about clippy's `--all-targets` "slightly different region/line counts" is the surface-level effect of the same cause: clippy's separate compile contributes its own instrumentation regions, which is what `cargo-llvm-cov report` reflects.

### Why we're accepting it

The big-doubling case (clippy in `target/debug/` with `--all-features`, then test-ci in `target/llvm-cov-target/` with mock features) was the ~50 s saving the show-env work captured. The remaining clippy-vs-build dual is smaller (~20–30 s of CPU on cold), upstream-cargo-design-bound, and the available local fixes (drop clippy from CI; split into parallel jobs; replace clippy with check) each carry a coverage / safety-net trade-off larger than the saving. The trigger conditions to re-open are in the **Risks / unknowns** section above.

## Discussion log

Append findings as we go. Keep terse — full data lives in CI logs.

- **2026-05-09** — plan drafted. No steps started yet.
- **2026-05-09 — steps 1–2** (centralize `ci_features`): pure refactor, four call sites unified (`test-ci` × 2, `_cov-collect` × 2). Side-fix: `_cov-collect`'s doc-test step was still on `--all-features`, missed in the previous sprint's `test-ci` fix; aligned to `ci_features` for consistency. Behavior preserved (`just test-ci` passes, coverage 52.48%, ratchet baseline 52).
- **2026-05-09 — step 3** (`show-env` single build): swapped `test-ci` to a multi-line bash recipe that `eval`s `cargo llvm-cov show-env --export-prefix`, then runs clippy, nextest, doc-test, and the two report invocations under the same exported env. All artifacts share `target/llvm-cov-target/`. Cold local wall: **97 s** (vs ~100 s for the previous test-ci alone, BEFORE separately running `just clippy`'s ~50–60 s on top). 328/328 tests pass. Coverage shifted 52.48% → 52.27% — likely from clippy's `--all-targets` instrumented compile producing slightly different region/line counts than nextest alone; still solidly above the 52 ratchet, no test count change. Will see the real CI delta on next push (CI's 2-core runner gives a much bigger asymmetry than the 32-core local box).
- **2026-05-09 — step 6** (CI measurement, post-show-env, cargo-about still installed): on warm CI (cache hit from prior run), Test step dropped from **14m 4s → 6m 27s** vs the most recent green main run; combined Clippy+Test phase 16m 45s → 6m 27s (-10m 18s, -61%). The bigger-than-expected warm win comes from the cache now storing instrumented artifacts: previously, the cache held non-instrumented `target/debug/` from the separate clippy step, so the test-ci step always paid full instrumented-compile cost; now the cache itself is the instrumented `target/llvm-cov-target/` and warm restores actually warm the path test-ci hits.
- **2026-05-09 — bonus: drop cargo-about from CI install**: warm-CI Test step dropped further **6m 27s → 3m 15s** (-3m 12s) on the next run. Estimate was ~10–30 s; reality was an order of magnitude bigger. Most of it isn't the cargo-about *install* (~5 s in Setup) but the build-script invocation itself: `cargo-about generate --workspace --all-features --locked --fail` walks the full 820-dep license graph every cold build, and the 2-core CI runner feels that as raw wall time. Build-script falls back to a stub-blob HTML when cargo-about isn't on `$PATH` (`Err(e)` branch in `generate_third_party_licenses`), and CI tests check About-pane structure, not embedded content. **Cumulative warm-CI saving across the sprint: ~13.5 minutes (-81%).**

## Out of scope

- Splitting CI into parallel jobs (option D in the original survey). Real projects (tokio, ripgrep) do this, but it duplicates Swatinem cache and fights for the 10 GB cache budget. Reconsider if PR cycle time becomes pressing or if the workspace grows past what one job can hold.
- `sccache` with remote backend. Setup overhead vs. payoff is poor for a single ubuntu-latest job.
- `cargo-hack` feature-graph testing. Useful in principle but each features-set materializes its own target dir; would compound the disk problem we're trying to solve.

## References

- [taiki-e/cargo-llvm-cov `show-env`](https://github.com/taiki-e/cargo-llvm-cov?tab=readme-ov-file#get-coverage-of-cc-code-linked-to-rust-libtest-binary) — the `show-env --sh` invocation is the contract this plan rests on.
- [cargo issue #8716](https://github.com/rust-lang/cargo/issues/8716) — RUSTFLAGS in fingerprint, the upstream constraint that forces this restructure.
- Upstream cargo work on the broader artifact-sharing problem (status of these affects when this sprint's workaround can retire):
  - [Rework Cargo Build Dir Layout (issue 401, 2025h2, closed)](https://github.com/rust-lang/rust-project-goals/issues/401) — read the comment thread for the actual shipping cadence.
  - [Cargo cross workspace cache (issue 626, 2026, open)](https://github.com/rust-lang/rust-project-goals/issues/626).
  - [2026 goal doc](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html).
  - [cargo#5931 — implementation tracking issue](https://github.com/rust-lang/cargo/issues/5931).
- Sibling sprint: [`build-time-optimization.md`](build-time-optimization.md).
