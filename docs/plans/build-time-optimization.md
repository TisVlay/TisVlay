# Build-time optimization sprint

> **Status:** sprint complete 2026-05-09. One real win shipped (knob #1 — profile tweaks: cold -11 %, incremental -22 to -32 %, `just check` -7 %). One CI hardening landed (knob #3 — `cache-on-failure: true`). One hygiene win (knob #4 — `cargo machete` cleanup, no time impact). The other six knobs were either dropped after the data invalidated their premise (parallelism makes most CPU-time wins not translate to wall time) or deferred to `docs/ideas/`.
>
> | Knob | Status |
> |---|---|
> | #1 — `[profile.dev/test]` debuginfo tweaks | ✅ shipped |
> | #2 — deps `opt-level = 1` | ❌ dropped (regression) |
> | #3 — CI cache hardening | ✅ shipped |
> | #4 — `cargo machete` cleanup | ✅ shipped (hygiene) |
> | #5 — Slint feature audit | ⏭ no change needed |
> | #6 — `tisvlay-gui` cfg(test) move | ❌ dropped (parallelism) |
> | #7 — `tisvlayd` test consolidation | ⏸ deferred to ideas |
> | #8 — `tisvlay-core` split | ❌ dropped (high effort, marginal) |
> | #9 — mold linker | ❌ dropped (rust-lld already default on 1.95) |
> | #10 — drop X11 from GUI | ✅ shipped (CI bet — 90 s CPU saved, wall flat locally, expected 20–40 s wall on CI) |

Speed up the cargo edit-build-test loop on this 11-crate workspace without sacrificing test coverage or regressing release artifacts. Local dev box is 32-core / 188 GB; CI runs on GitHub Actions `ubuntu-latest` (2 cores / ~7 GB RAM / ~14 GB disk). The pain points we want to move are **cold `just check` (105 s)**, **`just test-ci` (104 s)**, and **incremental rebuild after a bottom-crate edit (~9 s)**. Single-bin relink is already 0.6 s — that knob is closed.

## Baseline (rustc 1.95, edition 2024, resolver v3)

Captured by `scripts/bench_build_baselines.py`; raw outputs land in gitignored `bench-results/baseline/` (`results.json`, `timings/*.html`, `logs/*`).

| Step | Wall | CPU-time | Parallelism (CPU/wall) |
|---|---:|---:|---:|
| Cold `cargo build --profile=test --workspace --all-targets --all-features` | 57.3 s | 674 s | 11.8× |
| Incremental after `tisvlay-core/src/lib.rs` edit | 8.8 s | 87 s | 9.9× |
| Incremental after `tisvlay-cli/src/main.rs` edit (cascade-tainted)¹ | 10.0 s | 103 s | 10.3× |
| Cold `just check` (deny + fmt + build + clippy + test) | 105.4 s | — | — |
| Cold `just test-ci` (`cargo-llvm-cov` instrumentation) | 103.8 s | — | — |
| Single-bin relink (touch `tisvlay-cli/main.rs`, `cargo build --bin tisvlay`) | 0.62 s | — | — |

¹ The benchmark's tisvlay-core revert touched its mtime, cascading into the leaf measurement. A genuine leaf-only relink is the 0.62 s line above.

### Critical-path units in the cold build

Top wall-time per single compilation unit (longest is the lower bound on cold build time, no matter how many cores we throw at it):

| Crate / unit | Wall (s) |
|---|---:|
| `tisvlay-gui` lib (no `cfg(test)`) | 19.4 |
| `tisvlay-gui` lib (with `cfg(test)`) — second variant | 17.9 |
| `x11rb-protocol` | 10.9 |
| `moxcms` (×2) | 10.1 + 9.7 |
| `read-fonts` (×2) | 7.6 + 6.9 |
| `tisvlayd` lib (with `cfg(test)`) | 7.4 |
| `wayland-protocols` | 6.7 |
| `tisvlay-gui` `build.rs` (slint-build `compile()`) | 6.3 |
| `i-slint-compiler` | 5.7 |

By aggregate CPU-time the top three crates are `tisvlayd` (66 s, 12 integration test binaries), `tisvlay-gui` (51 s, dual-compile + build script), `tisvlay-session` (23 s).

**Implication:** the 32-core machine is already critical-path-bound, not parallelism-bound. The cold-build floor is roughly the longest serial chain through the dep graph (`moxcms → i-slint-compiler → tisvlay-gui lib → tisvlay-gui lib (test)` ≈ 50 s). To cut below 50 s cold, we have to either shrink one of those units or kill a duplicate compilation.

## Methodology

The benchmark script captures both wall-clock and `cargo --timings` HTML for each step. Re-run flow when evaluating a knob:

```
nix shell nixpkgs#python3 --command python3 scripts/bench_build_baselines.py
```

Outputs go to `bench-results/baseline/` (overwriting). Before applying a knob, **rename the existing dir** so we can diff against it later:

```
mv bench-results/baseline bench-results/before-<knob-id>
# ... apply the knob ...
nix shell nixpkgs#python3 --command python3 scripts/bench_build_baselines.py
mv bench-results/baseline bench-results/after-<knob-id>
```

Record the wall-time delta (and any artifact-size delta) in the experiments table below.

For the `cargo --timings` HTML, scrape `const UNIT_DATA` with `scripts/parse_timings.py` (todo: promote `/tmp/parse_timings.py` to the repo if we keep iterating).

## Findings that reshape the knob list

These were on the original list but the data argues against them, given our hardware and rustc version:

- **mold or external lld swap** — rustc 1.95 already defaults to `rust-lld` on `x86_64-unknown-linux-gnu`. Single-bin relink is 0.6 s, so even a 50% faster linker only saves a few hundred ms locally. Worth re-checking if a CI profile shows linker-dominated cold builds; otherwise skip.
- **`-Zthreads=N` parallel rustc frontend** — we already get 12× parallelism on cold builds and are critical-path-bound, not core-bound. Plus it's nightly-only.
- **`cargo-hakari` workspace-hack** — the published wins are on workspaces with hundreds of crates. We have 11.

## Experiments

Knobs ordered by expected ROI for the cold `just check` / `just test-ci` paths. Update **Status** (`todo` / `wip` / `done` / `dropped`) and **Measured** as we go.

| # | Knob | Expected (cold / CI) | Effort | Risk | Status | Measured |
|---|---|---|---|---|---|---|
| 1 | `[profile.test] split-debuginfo = "unpacked"` + `debug = "line-tables-only"`; `[profile.dev] split-debuginfo = "unpacked"` (full DWARF preserved on dev for `cargo run`) | -3–8 s / -10–20 s | trivial | none | done | cold -6.2 s (-10.8%), incr-core -1.9 s (-22%), incr-leaf -3.2 s (-32%), `just check` -6.9 s (-6.6%), `just test-ci` -3.4 s (-3.3%) |
| 2 | `[profile.dev.package."*"] opt-level = 1` (faster test runtime, marginal compile cost) | ~0 / ~0 (test runtime only) | trivial | low | dropped | cold +30 s (+60%), `just check` +51 s (+51%), `just test-ci` +61 s (+61%); incremental wins <1 s — net loss for this workload |
| 3 | Verify `Swatinem/rust-cache@v2` is wired correctly in `.github/workflows`; key by rustflags so coverage and non-coverage caches don't collide | n/a / huge on warm CI | low | low | done | already wired (`ci.yml:46`); added `cache-on-failure: true`; rustflags-keying not needed (cargo-llvm-cov uses a separate `target/llvm-cov-target/`, so coverage and non-coverage artifacts coexist) |
| 4 | `cargo machete` audit — strip any unused workspace deps it flags | unknown | trivial | low | done | dropped 3 unused deps from `tisvlay-gui` (`directories`, `serde`, `toml`); 2 false positives ignored via `[package.metadata.cargo-machete]`. Build-time impact within noise (cold ±0.5 s, incr +0.3 s, `just check` -0.5 s) — value is hygiene, not seconds |
| 5 | Slint feature audit — confirm `compat-1-2` is needed; check whether `backend-default` pulls more than femtovg requires | -5–10 s / -5–10 s | low | low | dropped | nothing to trim. `backend-default` resolves to `backend-winit` for Linux (functionally equivalent, no extra cost). `compat-1-2` is an empty marker feature (no deps). The slow units (`moxcms` 20 s, `read-fonts` 14 s, `x11rb-protocol` 11 s) are intrinsic — `image` is required by `slint-compiler` for asset embedding, `fontique`/`read-fonts` is required by `i-slint-common`, and `x11rb` is pulled by both `arboard` (text clipboard) and Slint's winit. Trimming any of those means dropping a user-facing feature, not just a build-time tweak |
| 6 | Move `cfg(test)` modules out of `tisvlay-gui/src/lib.rs` into `tests/` to kill the lib's dual-variant compile (19.4 s + 17.9 s today) | -10–15 s / -10–20 s | medium | medium | dropped | invalidated by timings data: both lib variants start at t≈29.3 s and run in parallel; total wall is determined by the *longer* of the two, not the sum. Eliminating the test variant saves CPU only, not wall — on 32 cores or 2 cores. Refactor cost not justified |
| 7 | Consolidate `tisvlayd` integration tests (12 files → ~4 logical groups) so we link fewer test binaries cold | -5–10 s / -10–15 s | medium | medium (test isolation) | deferred | timings show all 12 link in parallel locally (whole batch ~5 s wall); estimated CI saving 5–10 s, local 0–2 s. Refactor cost not justified yet — punted to [`docs/ideas/tisvlayd-test-consolidation.md`](../ideas/tisvlayd-test-consolidation.md) with trigger condition |
| 8 | Split `tisvlay-core` into `core-types` + `core-dbus` so non-dbus changes don't cascade through the dbus client surface | minimal cold; helps incr-after-core | high | medium | dropped | only ~20% of `tisvlay-core` edits touch dbus surface (most edits hit shared types/state), so ~80% of edits would still trigger the full cascade. Estimated saving on the helped 20% is 1–2 s of an already-7 s incremental. Multi-hour refactor for a marginal subset of edits |
| 9 | Re-evaluate `mold` once knobs 1–6 are in (CI may still benefit even if local doesn't) | -? / -? | low | low | dropped | rust-lld is the rustc 1.95 default on `x86_64-unknown-linux-gnu` (verified by `cargo build -vv` showing no `-fuse-ld=` override); single-bin relink is already 0.6 s. mold's published 10–15% advantage over lld on big links translates to ~0.1 s per relink for our binary sizes. Add later if CI link-time profiling shows it dominates |
| 10 | Drop X11 from the GUI: swap `slint = ["backend-default"]` → `["backend-winit-wayland"]`, swap `arboard` → `wl-clipboard-rs`, fix `i-slint-backend-selector` to `default-features = false` so `dev-mcp` doesn't re-enable x11rb under `--all-features` | -? cold local / -20–40 s `just test-ci` on 2-core CI (estimate) | medium | medium (X11 users lose the GUI) | done | x11rb / x11rb-protocol / x11-clipboard / x11-dl all gone from the dep tree under both default and `--all-features`. CPU-time saving 674 → 584 s (-90 s, -13%). Wall-time: cold flat at 50 s, `just check` -0.4 s, `just test-ci` -0.2 s on 32-core local. The CI win is unmeasured locally but should materialize on 2-core ubuntu-latest where the parallelism that hides the saving here collapses |

After each knob lands, record the new cold / incr-core / `just check` / `just test-ci` numbers. If a knob has no measurable effect, mark **dropped** and write a short note in the discussion log below.

## Discussion log

Append findings as we go (one bullet per knob attempt). Keep it terse — full data lives in `bench-results/`.

- **2026-05-08** — baseline captured; cold = 57.3 s, just check = 105.4 s, just test-ci = 103.8 s. Critical path identified at `tisvlay-gui` (dual compile) + `moxcms` + `read-fonts` chain. Linker is rust-lld default; relink already 0.6 s.
- **2026-05-09 — knob #1** (profile tweaks): cold 57.3 → 51.1 s (-10.8%), incr-after-core 8.8 → 6.9 s (-22%), incr-after-leaf 10.0 → 6.8 s (-32%), `just check` 105.4 → 98.5 s (-6.6%), `just test-ci` 103.8 → 100.3 s (-3.3%). Single run, no noise estimate; the incremental wins are solidly above run-to-run variance. `target/debug` size also drops because `.o` files no longer carry inlined debuginfo (didn't measure exact bytes). The justfile's `CARGO_PROFILE_DEV_DEBUG=line-tables-only` env-var override on `_cov-collect` / `test-ci` still applies because `cargo llvm-cov` uses the `dev` profile (where we left full debug for lldb sessions); leaving those overrides in place.
- **2026-05-09 — knob #2** (deps `opt-level = 1`): **dropped, net loss.** Cold build 51.1 → 81.5 s (+60%), `just check` 98.5 → 149.1 s (+51%), `just test-ci` 100.3 → 161.4 s (+61%). Incremental rebuilds shaved <1 s. Rationale for dropping: our tests are I/O- and D-Bus-bound, not CPU-bound, so the runtime upside doesn't materialize. The compile-time cost is real and large because `--all-features` + workspace breadth means a lot of registry crates (rustls, serde_json, image, regex, the Slint stack) get re-LLVM'd at -O1 on every cold build. Reverted; profile config is back to knob #1 state.
- **2026-05-09 — knob #3** (CI cache verification): cache was already wired (`Swatinem/rust-cache@v2`). Added `cache-on-failure: true` so a flaky test or ratchet fail no longer wipes the warm target/ tarball. The original "key by rustflags" concern dropped — `cargo-llvm-cov` writes to its own `target/llvm-cov-target/`, so coverage and non-coverage artifacts coexist in the cache without collision. CI-only knob; can't measure locally, will see the impact on the next failed-build re-run.
- **2026-05-09 — knob #4** (`cargo machete` audit): flagged 5 deps in `tisvlay-gui`. Three (`directories`, `serde`, `toml`) were truly unused — removed. Two false positives kept and silenced via `[package.metadata.cargo-machete]`: `gettext-rs` (Cargo name uses `-`, Rust import is `gettextrs`), `i-slint-backend-selector` (feature-gated `dev-mcp`, registered at link time, no `use …` ever appears). Build-time impact within run-to-run noise (~±0.5 s on cold paths) — three removed deps were already used elsewhere in the workspace, so the dep graph didn't actually shrink. Value is hygiene + a clean machete run on future audits.
- **2026-05-09 — knob #5** (Slint feature audit): nothing to trim. `backend-default` is functionally equivalent to `backend-winit` for Linux (resolves to the same `selector/backend-winit` underneath). `compat-1-2` is an empty marker feature with no actual deps. The slow Slint units (`moxcms` 20 s, `read-fonts` 14 s, `x11rb-protocol` 11 s) are pulled by `slint-compiler`, `i-slint-common`, and `arboard`/`winit` respectively — all on paths we can't drop without giving up image assets in `.slint` files, font rendering, or text clipboard support.
- **2026-05-09 — knob #6** (move `cfg(test)` out of `tisvlay-gui` lib): **dropped after timings analysis.** The two lib variants (`lib` and `lib (test)`) both start at t≈29.3 s of the cold build and complete in parallel by t≈47–49 s. Eliminating one saves ~17.9 s of CPU but ~0 s of wall — cargo already runs them concurrently. Same conclusion holds on a 2-core CI runner because each variant is one rustc process, and two rustc processes still saturate two cores. Medium-effort refactor for a CPU-only saving doesn't pay off.
- **2026-05-09 — knobs #7 / #8 / #9** (test consolidation, core split, mold linker): all three turned out the same shape on inspection. Knob #7's 12 tisvlayd test binaries link in parallel (whole batch ~5 s wall locally), so consolidation is mostly a CI bet — punted to [`docs/ideas/tisvlayd-test-consolidation.md`](../ideas/tisvlayd-test-consolidation.md) with a trigger condition. Knob #8's `tisvlay-core` split would only help the ~20% of edits that touch dbus surface, by 1–2 s each; the multi-hour refactor isn't earning that. Knob #9 is moot because rustc 1.95 already uses rust-lld by default and our biggest relink is 0.6 s — mold's win is sub-second.
- **2026-05-09 — knob #10** (X11 drop): added late after the user noted `tisvlay-session` is already Wayland-best-effort. Three changes: (1) `slint` features `backend-default` → `backend-winit-wayland`, (2) replaced `arboard` (which has an unconditional Linux `x11rb` dep) with `wl-clipboard-rs`, (3) added `default-features = false` to our optional `i-slint-backend-selector` dep so `dev-mcp` (and thus `--all-features`) doesn't silently re-enable x11rb via the selector's `default = ["backend-winit"]` (which is x11+wayland). Verified `cargo tree` shows no `x11rb` or `x11rb-protocol` in the dep graph under any feature combination. CPU time per cold build dropped 674 → 584 s (-90 s, -13%) — but wall time on the 32-core local box is flat because x11rb-protocol's 10.9 s ran *concurrently* with other slow units (moxcms, read-fonts, slint-compiler) and was never on the critical path. The CI win is the bet: on 2-core ubuntu-latest where parallelism collapses, the 90 s of CPU savings should translate more directly to wall time. Will see the actual delta on the next CI run. Trade-off: X11 users completely lose the GUI window (tray + CLI + daemon still work for them).

## Out of scope (for this sprint)

- Cranelift codegen backend — still nightly-only as of May 2026, missed 2025H2 stabilization target.
- Larger CI runner / self-hosted — paid; revisit only if all software knobs underperform expectations.
- Replacing Slint — owns the GUI; not a build-time decision.
- `cargo-mold-binstall` or any binstall path — Nix-managed toolchain; we'd add to `flake.nix` if we ever decide we need mold.

## References

- Baseline raw data: `bench-results/baseline/` (gitignored)
- Bench script: `scripts/bench_build_baselines.py`
- Related deferred work: `docs/ideas/gui-test-coverage-fanout.md` (its "candidate optimizations to land first" overlaps experiments 1, 7).
