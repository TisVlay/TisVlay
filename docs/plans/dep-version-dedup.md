# Third-party multi-version dep cleanup

> **Status:** sprint complete 2026-05-09. Replaced `vergen-gix` with a slim hand-rolled `build.rs` (lockfile 820 → 717 crates, -12.5%; cold CI Test step 13m 53s → 10m 6s, -27%; warm unchanged) and added a `cargo-deny [bans] multiple-versions = "warn"` config with a 14-entry skip-list documenting upstream-bound cases. Steps + measurements in the table below.
>
> | Step | Status |
> |---|---|
> | 1 — hand-rolled `build.rs` env-var emitter | ✅ shipped |
> | 2 — drop `vergen-gix` from `[build-dependencies]` | ✅ shipped (-103 crates) |
> | 3 — verify `hashbrown` dups gone | ⏭ partial (sources moved upstream-bound) |
> | 4 — measure cold + warm `just test-ci` | ✅ -27% cold, warm flat |
> | 5 — `[bans]` config in `deny.toml` with skip-list | ✅ shipped |
> | 6 — `just deny` recipe + CI | ✅ shipped |
> | 7 — docs pointer in `docs/licensing.md` | ✅ shipped |

Continuation of the build-time work in [`ci-shared-target.md`](ci-shared-target.md) and [`build-time-optimization.md`](build-time-optimization.md). After the share-target sprint shipped (-81% warm-CI Test+Clippy phase), the remaining workspace-side fat is third-party multi-version compilation — `cargo tree --workspace --all-features --target x86_64-unknown-linux-gnu --duplicates --depth 0` shows 20 crates with multiple distinct versions pinned in the lockfile. Each version is a separate compile unit; on the 2-core CI runner that's real wall time.

The original framing in `build-time-optimization.md` (footnote in the **Critical-path units** section) called this "fundamentally out of our control — different transitive deps pin different versions." That was correct as far as it went, but a focused trace shows three concrete sources and two of them are addressable.

## Findings

### Where our duplicates actually come from

| Source | Pulls in | Cascaded duplicate versions | Our control |
|---|---|---|---|
| **`vergen-gix`** (tisvlay-gui build-dep — 46 gix-* crates) | gitoxide ecosystem | `hashbrown 0.14` (via `dashmap@6` → `gix-tempfile`); `hashbrown 0.15` (via `imara-diff` → `gix-diff`); both versions are internal to the gix stack | **Replaceable** — we own this dep |
| **`smithay-clipboard 0.7.3`** (transitive via Slint's `copypasta`) | both SCTK 0.19 *and* SCTK 0.20 internally — that single crate has incompatible internal pins | `calloop 0.13`, `rustix 0.38`, `thiserror 1`, `linux-raw-sys 0.4`, `signal-hook 0.3` — all from the SCTK 0.19 path | Upstream-bound — wait |
| **`rustls`/`ring`** (network stack) | `getrandom 0.2.17` (ring's pin) | the only path to old getrandom for us | Upstream-bound — or swap rustls's crypto backend |

The other 17 multi-version crates in `cargo tree -d` (`tiny-skia`, `pastey`, `unicode-width`, `webpki-roots`, `zune-core`, etc.) are downstream effects of these three sources or are tiny crates with negligible compile cost.

### What's NOT in cargo's toolbox

Confirmed by the 2025–2026 SOTA survey. cargo's resolver does SAT-style backtracking but **does not have a "minimize duplication" mode**. RFC 3692 is about feature unification, not version. Issue [cargo#2064](https://github.com/rust-lang/cargo/issues/2064) is about public/private deps. The 2025h2 build-dir layout work and 2026 cross-workspace-cache goal are about cache layout, not resolution.

So unifying versions is purely a *manual* `cargo tree -i` → identify lagging parent → `cargo update -p` → repeat workflow. There's no auto-solver that says "this set of upgrades unifies version X."

### Anti-patterns flagged

- **`[patch.crates-io]` to force unification** — only works if the patched version satisfies all transitive semver bounds. If `dashmap@6` pins `hashbrown ^0.14` and we patch to 0.16, dashmap fails to compile. Cascades into vendoring `dashmap` itself. Not worth it.
- **`cargo-hakari`** — solves *feature* fragmentation across a workspace; doesn't reduce version duplication.
- **`[workspace.dependencies]` hoisting** — Cargo.toml text-level dedup; doesn't change resolver behavior.
- **`-Zminimal-versions`** — different problem (lower-bound auditing, not duplication).

## Approach

### Part 1 — Replace `vergen-gix`

`tisvlay-gui/build.rs` uses `vergen-gix` for three env vars:

- `VERGEN_CARGO_TARGET_TRIPLE` — already in `cargo:rustc-env=` accessible cargo build env (`TARGET`).
- `VERGEN_RUSTC_SEMVER` — `rustc --version` output, parseable to `MAJOR.MINOR.PATCH`.
- `VERGEN_GIT_SHA` (7-char short) — `git rev-parse --short HEAD`, with `VERGEN_IDEMPOTENT_OUTPUT` sentinel fallback when `.git/` is missing (release tarball / Nix sandbox / vendored build).

A 30-line hand-rolled `build.rs` covers all three with `Command::new("git")` and `env::var("TARGET")` + a parsed `rustc -V`. None of vergen-gix's gitoxide internals are required for our use case. The fallback semantics (sentinel string when git is unavailable) are easy to replicate.

Estimated savings: **eliminate 46 gix-* crates from the build**, remove both `hashbrown 0.14` and `hashbrown 0.15` from the dep tree (gix is the only source of either in our workspace), shrink the cold-build dep graph meaningfully. Will measure after.

Trade-offs:
- vergen-gix's pure-Rust gitoxide backend works in environments where `git` isn't on `$PATH`. Our build environments (Nix dev shell, distro CI, GitHub Actions) all have `git` available, but it's worth keeping a graceful fallback (the sentinel) so vendored builds don't break.
- Future env-var additions (e.g., `VERGEN_GIT_COMMIT_DATE`) would have to be hand-rolled too. Acceptable cost for a slim build script.

### Part 2 — `cargo-deny multiple-versions = "warn"` with skip-list

Existing `packaging/licensing/deny.toml` covers `licenses` only. Add a `[bans]` section that warns on multiple versions but skips the upstream-bound offenders. Skip-list seeded from current state:

- `hashbrown` (post-vergen-gix-removal, only one upstream source remains; skip if it's still showing up)
- `getrandom` (rustls/ring)
- `rustix`, `thiserror`, `calloop`, `linux-raw-sys`, `signal-hook` (smithay-clipboard 0.19/0.20 split)
- `tiny-skia` (resvg vs sctk-adwaita)
- `winit`, `zvariant`, `zbus_macros`, `wayland-client`, etc. — proc-macro vs runtime split, fundamental to cargo

The `skip` mechanism is per-crate with a reason field. Each entry should cite the upstream issue or transitive consumer that gates the dedup, so future contributors can re-evaluate when upstream moves.

CI integration: `prek` already runs `just deny`; the bans check joins the licenses check naturally. Output is a warning, not a failure — keeps the gate non-blocking while surfacing regressions.

## Steps

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Hand-roll a slim `build.rs` git-rev / target / rustc env-var helper for `tisvlay-gui` | done | ~25 lines covering all three env vars: `TARGET` from cargo's env, `rustc -V` parsed for semver, `git rev-parse --short=7 HEAD` with the `VERGEN_IDEMPOTENT_OUTPUT` sentinel preserved for vendored builds. `cargo:rerun-if-changed=.git/HEAD` for branch-switch invalidation. |
| 2 | Drop `vergen-gix` from `tisvlay-gui/Cargo.toml` `[build-dependencies]` | done | Lockfile 820 → 717 crates (-103, -12.5%); 46 gix-* crates and dashmap/imara-diff/etc. all gone. |
| 3 | Verify `cargo tree -d` no longer shows `hashbrown 0.14` or `hashbrown 0.15` | done — partial | Both versions still present, but the *sources moved*: 0.14 now from `i-slint-compiler` → `rowan` (Slint internals), 0.15 from `prost-build` → `petgraph` (i-slint-backend-testing's `mcp` feature). Both upstream-bound now; the gix-internal multi-pin is the part we actually removed. |
| 4 | Re-run cold + warm `just test-ci` locally; record dep-graph size + wall delta | done | Cold local 97 s → 94 s (~3 s saving on 32-core, near noise — most of the dropped CPU work was deps that fit inside parallel slack). 328/328 tests pass; coverage 52.27% → 52.41%. CI saving is the bet — 2-core runner serializes more, so the 12.5% dep-count drop translates more directly to wall. |
| 5 | Extend `packaging/licensing/deny.toml` with `[bans]` section: `multiple-versions = "warn"` + curated `skip` list with per-entry rationale | done | 14 skip entries, each with rationale: hashbrown / getrandom / smithay-clipboard cascade (8 entries) / tiny-skia split / 6 small-cost transitives. cargo-deny flagged 4 candidates as "unnecessary skip" or "unmatched skip" because its default-target view doesn't see them as duplicates (target-conditional deps); those were dropped to avoid noise. |
| 6 | Update `just deny` recipe to include the `bans` check + push to CI | done | Recipe now runs `cargo deny check ... licenses bans`. Local `just deny` clean (`bans ok, licenses ok`). |
| 7 | Document the routine in `docs/licensing.md` pointing at the deny.toml entries | done | One-sentence pointer added under the **Pipeline** section, linking back to this plan for the full rationale. |

## Risks / unknowns

- **Hand-rolled build script's fallback path** — if `git rev-parse` fails (no `.git/`, missing binary), the sentinel string must match what the diagnostic-blob renderer treats as "absent" (today: `"VERGEN_IDEMPOTENT_OUTPUT"` matched in `diagnostic.rs::maybe_git_sha`). Need to keep the same string contract.
- **`cargo-deny check bans` runtime cost** — `bans` runs the dep graph; on warm CI it's fast (<2 s). Should not move the needle on test-ci wall.
- **Future Slint upgrade** — Slint 1.17+ may upgrade copypasta's `smithay-clipboard` to a version that drops the SCTK 0.19 path. When that lands, the skip-list gets shorter. Re-audit on every Slint bump.

## Discussion log

Append findings as we go. Keep terse — full data lives in CI logs and the research output.

- **2026-05-09** — plan drafted. No steps started yet. Local audit ran via `cargo tree --duplicates`; SOTA research run synthesized into the **Findings** section.
- **2026-05-09 — steps 1–4** (vergen-gix replacement): hand-rolled `tisvlay-gui/build.rs` to drop `vergen-gix` and its 46-crate gitoxide ecosystem. Lockfile 820 → 717 crates (-103, -12.5%). Cold local `just test-ci` 97 s → 94 s; tests still pass (328/328); coverage 52.27% → 52.41%. The hashbrown 0.14 / 0.15 duplicates remained but moved sources (now `rowan` / Slint compiler and `petgraph` / `prost-build` respectively, both upstream-bound) — the win was eliminating the gix-internal multi-pin and the broader dep weight, not unifying hashbrown. Real saving lands on 2-core CI where the dep-count drop serializes less.
- **2026-05-09 — CI measurement**: cold Test step **13m 53s → 10m 6s (-27%)**; warm Test step 3m 15s → 3m 23s (within noise). The asymmetry is what we'd expect: deleting transitive deps wins where compilation happens (cold cache miss), barely shows where artifacts are restored from a tarball (warm). Combined with the share-target sprint's warm-CI saving, the two now cover both axes — share-target hits warm hardest (target dir layout change), vergen-gix removal hits cold hardest (fewer crates to compile).
- **2026-05-09 — steps 5–7** (`cargo-deny [bans]` config + docs): added `[bans] multiple-versions = "warn"` with a 14-entry skip-list documenting the upstream-bound multi-version cases. `just deny` recipe extended to run `licenses bans`. Local clean (`bans ok, licenses ok`). cargo-deny flagged 4 of my initial skip entries as "unnecessary skip"/"unmatched skip" because its default-target resolver view doesn't see them as duplicates (target-conditional deps); dropped those entries to avoid noise. `docs/licensing.md` Pipeline section gained a one-sentence pointer to this plan for the rationale.

## Out of scope

- **`cargo-dedupe`** (Oct 2025, v0.0.1). Auto-rewrites direct-dep version strings to unify. Useful for workspaces with many direct deps pinned to old versions; ours is small enough that manual `cargo update -p` is cheaper than learning the tool. Revisit if the dep list grows.
- **Forking `smithay-clipboard` to drop the SCTK 0.19 path.** Last resort; routine maintenance overhead exceeds the saved CI time at our scale.
- **Switching `rustls` to the `aws-lc-rs` crypto backend.** aws-lc-rs has its own heavy deps (FFI to AWS-LC C library); the net dep-graph delta isn't obviously a win and the change is invasive (build-script C compilation, libssl-style packaging questions).
- **`-Zminimal-versions` / `cargo +nightly`** — different problem space; we're on stable.

## References

- Local audit: `cargo tree --workspace --all-features --target x86_64-unknown-linux-gnu --duplicates --depth 0` (run before applying steps).
- 2025–2026 SOTA: SAT solvers, detection tools, mass-upgrade strategies — see the conversation that produced this plan.
- [`cargo tree` docs](https://doc.rust-lang.org/cargo/commands/cargo-tree.html) — `--duplicates` and `--invert` are the canonical workflow tools.
- [cargo-deny `[bans]` config](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html) — `multiple-versions = "warn"` and the per-crate `skip` list.
- [cargo-dedupe](https://crates.io/crates/cargo-dedupe) — Oct 2025; out-of-scope for this plan but worth knowing.
- [Cargo issue #2064](https://github.com/rust-lang/cargo/issues/2064) — public/private deps; commonly conflated with version unification but actually different.
- [vergen-gix](https://docs.rs/vergen-gix) — the dep we're replacing.
- Sibling sprints: [`ci-shared-target.md`](ci-shared-target.md), [`build-time-optimization.md`](build-time-optimization.md).
