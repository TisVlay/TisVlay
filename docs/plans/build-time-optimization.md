# Build-time optimization sprint

> **Status:** baseline captured 2026-05-08, knobs identified, none applied yet. Track each experiment in the table at the bottom — update the **Status** and **Measured** columns as we ship each one.

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
| 2 | `[profile.dev.package."*"] opt-level = 1` (faster test runtime, marginal compile cost) | ~0 / ~0 (test runtime only) | trivial | low | todo | — |
| 3 | Verify `Swatinem/rust-cache@v2` is wired correctly in `.github/workflows`; key by rustflags so coverage and non-coverage caches don't collide | n/a / huge on warm CI | low | low | todo | — |
| 4 | `cargo machete` audit — strip any unused workspace deps it flags | unknown | trivial | low | todo | — |
| 5 | Slint feature audit — confirm `compat-1-2` is needed; check whether `backend-default` pulls more than femtovg requires | -5–10 s / -5–10 s | low | low | todo | — |
| 6 | Move `cfg(test)` modules out of `tisvlay-gui/src/lib.rs` into `tests/` to kill the lib's dual-variant compile (19.4 s + 17.9 s today) | -10–15 s / -10–20 s | medium | medium | todo | — |
| 7 | Consolidate `tisvlayd` integration tests (12 files → ~4 logical groups) so we link fewer test binaries cold | -5–10 s / -10–15 s | medium | medium (test isolation) | todo | — |
| 8 | Split `tisvlay-core` into `core-types` + `core-dbus` so non-dbus changes don't cascade through the dbus client surface | minimal cold; helps incr-after-core | high | medium | todo | — |
| 9 | Re-evaluate `mold` once knobs 1–6 are in (CI may still benefit even if local doesn't) | -? / -? | low | low | todo | — |

After each knob lands, record the new cold / incr-core / `just check` / `just test-ci` numbers. If a knob has no measurable effect, mark **dropped** and write a short note in the discussion log below.

## Discussion log

Append findings as we go (one bullet per knob attempt). Keep it terse — full data lives in `bench-results/`.

- **2026-05-08** — baseline captured; cold = 57.3 s, just check = 105.4 s, just test-ci = 103.8 s. Critical path identified at `tisvlay-gui` (dual compile) + `moxcms` + `read-fonts` chain. Linker is rust-lld default; relink already 0.6 s.
- **2026-05-09 — knob #1** (profile tweaks): cold 57.3 → 51.1 s (-10.8%), incr-after-core 8.8 → 6.9 s (-22%), incr-after-leaf 10.0 → 6.8 s (-32%), `just check` 105.4 → 98.5 s (-6.6%), `just test-ci` 103.8 → 100.3 s (-3.3%). Single run, no noise estimate; the incremental wins are solidly above run-to-run variance. `target/debug` size also drops because `.o` files no longer carry inlined debuginfo (didn't measure exact bytes). The justfile's `CARGO_PROFILE_DEV_DEBUG=line-tables-only` env-var override on `_cov-collect` / `test-ci` still applies because `cargo llvm-cov` uses the `dev` profile (where we left full debug for lldb sessions); leaving those overrides in place.

## Out of scope (for this sprint)

- Cranelift codegen backend — still nightly-only as of May 2026, missed 2025H2 stabilization target.
- Larger CI runner / self-hosted — paid; revisit only if all software knobs underperform expectations.
- Replacing Slint — owns the GUI; not a build-time decision.
- `cargo-mold-binstall` or any binstall path — Nix-managed toolchain; we'd add to `flake.nix` if we ever decide we need mold.

## References

- Baseline raw data: `bench-results/baseline/` (gitignored)
- Bench script: `scripts/bench_build_baselines.py`
- Related deferred work: `docs/ideas/gui-test-coverage-fanout.md` (its "candidate optimizations to land first" overlaps experiments 1, 7).
