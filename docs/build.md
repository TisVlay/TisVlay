# Build system

Developer-facing reference for how the workspace compiles, what gets pulled into the dep graph, and the conventions that keep the build fast on stable Rust + 2-core CI. Audience: someone touching the test pipeline, adjusting a profile, swapping a dep, or investigating "why does this take so long?". For the testing surface (mock backends, wire tests, observability) see [testing.md](testing.md); for license attribution + policy see [licensing.md](licensing.md); for architectural decisions see [design.md](design.md).

## Build pipeline

`just test-ci` is the canonical CI entry point. It runs the full check pipeline (clippy + nextest + doc tests + coverage report) under a single coverage-instrumented build, so all four stages share `target/llvm-cov-target/` instead of compiling the workspace twice, once plain and once instrumented.

The mechanism: `cargo llvm-cov show-env --sh` emits `export VAR=…` lines that route every later cargo command through cargo-llvm-cov's `RUSTC_WRAPPER`, which adds `-C instrument-coverage` and points `LLVM_PROFILE_FILE` into the target dir. `eval`-ing them in a multi-line bash recipe means clippy, nextest and the doc tests all share one instrumented build. The recipe pins `CARGO_TARGET_DIR=target/llvm-cov-target/` before the `eval`: show-env doesn't choose a directory itself, and cargo doesn't rebuild when only `RUSTC_WRAPPER` changes. Sharing `target/` would therefore reuse an uninstrumented `just build`, leaving no `.profraw` and a failing report.

There's a residual dual-compile that *can't* be eliminated on stable today (verified by `cargo clippy -v` rustc invocations): even with one target dir and one instrumentation setting, cargo treats `cargo clippy` and `cargo nextest run` as different "modes" with different artifact-emit flags. clippy emits `--emit=dep-info,metadata` (rmeta only — interface, no machine code); nextest emits `--emit=dep-info,metadata,link` (rmeta + `.rlib`). cargo's fingerprint distinguishes the two artifact kinds, so every workspace crate is recompiled in the second invocation. This is upstream cargo design — see "Review triggers" for the goal that may eventually fix it.

`tisvlay-gui` looks uniquely doubly-compiled when grepping for `Compiling X` lines because it's the only workspace crate with a `build.rs` — its build-script binary forces a "Compiling" verb in the clippy step where every other workspace crate gets "Checking" (rmeta-only). Cosmetic, not actual extra duplication.

### CI feature scope

CI lints and tests with `--all-features` — the same flags as local `prek`. The workspace's feature surface is small (core's `logging`/`wayland`, lg's `discovery`, cec's `test-link`; the mock backend is always compiled), so the instrumented all-features build fits the runner comfortably and there is exactly one feature configuration to reason about. The Slint MCP debug server is enabled ad-hoc with `--features slint/mcp` (a dependency feature, not a crate feature) and guards no workspace source.

## Lint policy

### Warnings are errors in the gates

Every gate recipe denies warnings: `just clippy`, `just doc` and `just test-ci` export `CARGO_BUILD_WARNINGS=deny` (cargo's `build.warnings`). So a lint at `warn` in `[workspace.lints]` blocks a commit and a CI run just as `deny` would.

- **Why `build.warnings` and not `-- -D warnings`:** it covers clippy, test and rustdoc builds with one setting. It also fails on warnings replayed from the cache, and toggling it doesn't invalidate the build cache, which passing `-D warnings` through clippy or `RUSTFLAGS` does. It doesn't cover build-script `cargo::warning=` lines or cargo's own manifest and filename-collision warnings.
- **Why per recipe and not `.cargo/config.toml`:** the Nix package build (`buildRustPackage`) reads that file with nixpkgs' rustc. A warning that a newer compiler adds would then break a NixOS rebuild. Local `cargo build` / `cargo run` stay warn-only, so a half-edited file still runs.
- **Rustdoc is a gate too.** `just doc` builds the workspace's API docs (`--no-deps`), so a broken intra-doc link, or a public doc linking a private item, fails like any other lint. In CI it runs in the Lint step's prek pass. The CLI's binary sets `doc = false`, since it shares the `tisvlay` name with the SDK library.

## Profile knobs

In the workspace `Cargo.toml`:

- `[profile.dev] split-debuginfo = "unpacked"` — keeps debug info in sibling `.dwo`/`.o` files instead of inlining it into the linked artifact. Relinks don't have to copy debuginfo, which is the dominant cost on incremental rebuilds. Linux default is "off" so we set it explicitly.
- `[profile.test] split-debuginfo = "unpacked", debug = "line-tables-only"` — `just check` and `just test-ci` use the test profile; `line-tables-only` shrinks artifacts (file:line in panic backtraces preserved, type info dropped).

`cargo run` still uses `dev` (full DWARF) for interactive lldb sessions.

## Build scripts

The GUI is split across two crates so the Slint-generated code — about 120k lines of Rust, compiled on one thread by rustc's front end — builds alongside the zenoh/SDK chain instead of after it. Each has a build script:

- [`tisvlay-gui/build.rs`](../crates/tisvlay-gui/build.rs) **compiles the Slint UI** via `slint_build::compile("ui/main.slint")`, generating the Rust code that `slint::include_modules!()` in that crate reads at compile time (plus the `lang/` catalogs' `.mo` files). The crate depends only on `slint`. Slint names the gettext domain after this package, which is why the UI crate holds the `tisvlay-gui` name.
- [`tisvlay-gui-app/build.rs`](../crates/tisvlay-gui-app/build.rs) belongs to the crate with the handwritten code and the `tisvlay-gui` binary (its library is still named `tisvlay_gui`), and does two things:

1. **Emit `VERGEN_*` env vars** for the diagnostic blob's `gui.{target,rustc,git_sha}` fields. Hand-rolled rather than via `vergen-gix`, which would pull tens of gitoxide crates for three env vars: target triple from cargo's `TARGET`, rustc semver parsed from `rustc -V`, git SHA from `git rev-parse --short=7 HEAD`. Failures emit the `VERGEN_IDEMPOTENT_OUTPUT` sentinel (vendored tarball, Nix sandbox without `.git/`, missing git binary); `diagnostic::maybe_git_sha` collapses that to JSON `null`.
2. **Generate the bundled-dep license HTML** via `cargo-about generate`. On `Err(NotFound)` (cargo-about not on `$PATH`) writes a stub-blob HTML so the build still succeeds. The About-pane "View notices" button shows install instructions in the stub case.

CI provisions `cargo-about` via `taiki-e/install-action` (the Setup step), so the `licenses-html` gate and `build.rs` both run the real generator; the stub fallback covers only ad-hoc builds outside CI / the dev shell where `cargo-about` isn't on `$PATH`. Shipped distro builds invoke `just licenses-html` from the Nix dev shell where `cargo-about` is provisioned. See [licensing.md](licensing.md) for the packaging path.

## Dep-graph posture

The workspace pulls roughly 700 transitive crates. Each *version* of a crate is a separate compile unit, so we keep an eye on multi-version pinning — on a 2-core CI runner that's real wall time. Goals: minimize duplicated versions, surface new duplicates without blocking PRs.

### `cargo-deny [bans]` config

[`packaging/licensing/deny.toml`](../packaging/licensing/deny.toml) has a `[bans] multiple-versions = "warn"` policy with a curated `skip` list. Each skip entry has a `reason` field citing the upstream consumer that gates dedup, so contributors don't waste time investigating known-and-accepted offenders.

Three known multi-version sources, in decreasing actionability:

- **Slint compiler internals** — `i-slint-compiler → rowan` pins `hashbrown 0.14`; `i-slint-backend-testing/mcp → prost-build → petgraph` pins `hashbrown 0.15`. Re-audit on Slint bump.
- **`smithay-clipboard 0.7.3` cascade** — that crate pulls *both* SCTK 0.19 and SCTK 0.20 internally; the SCTK 0.19 path cascades `calloop 0.13`, `rustix 0.38`, `thiserror 1`, `linux-raw-sys`, `signal-hook` duplicates. Pulled transitively via Slint's `copypasta`. Re-audit on Slint or `smithay-clipboard` upgrade.
- **`rustls`/`ring`** — keeps `getrandom 0.2`. Only path to old getrandom in our tree. Re-audit if `rustls` swaps its default crypto backend or `ring` upgrades.

Smaller offenders (`tiny-skia`, `foldhash`, `pastey`, `zune-*`, `dirs-sys`) trace back to the Slint stack or have negligible compile cost. Listed in the skip-list with a `reason` and reviewed periodically.

### `[graph] targets` filter

`[graph] targets = [{ triple = "x86_64-unknown-linux-gnu" }]` restricts cargo-deny's resolution to the platforms we ship for. Without it the default view considers every target triple and surfaces ~20 duplicate warnings for crates we never compile (`objc2-*` Apple, `windows-*` Windows, `jni-sys` Android, `r-efi`, `redox_*`).

`[graph]` is global — it affects `licenses` too. So a Linux-only license-policy gate won't catch an Apple/Windows-conditional one. That trade-off matches shipping reality: every binary we ship is Linux-only (D-Bus + systemd; the GUI is Wayland-only — see [design.md](design.md) "Known Challenges & Mitigations"). If we ever add a non-Linux release artifact, broaden the targets list.

### Default-feature resolution

cargo-deny resolves with default features rather than `--all-features`, matching the dep graph we actually distribute. The MCP debug stack (`i-slint-backend-testing/mcp`'s prost chain, with MPL-2.0 crates we don't audit for production) is reachable only via an ad-hoc `cargo run --features slint/mcp` — a `slint` dependency feature no workspace crate enables — so it's absent from both default- and all-feature resolution and never reaches the licenses gate.

cargo-deny 0.18+ also excludes dev-deps from `bans` and `licenses` by default — no extra `multiple-versions-include-dev` knob needed.

## Review triggers

Patterns to revisit when their gating condition fires:

- **`smithay-clipboard` cascade dedup** — re-audit cargo-deny skip entries on every Slint bump; a future `copypasta` upgrade may drop the SCTK 0.19 path.
- **`rustls`/`ring` getrandom** — re-audit when `rustls` upgrades its default crypto backend or when `ring` updates getrandom.
- **`vergen-gix` reintroduction** — only if `vergen-gix` ever drops the dashmap/imara-diff pins itself, or if `git` becomes unavailable on a build target we care about.
- **Cargo cross-workspace-cache (2026 goal)** — tracked in [concerns/test-ci-cargo-llvm-cov-show-env.md](concerns/test-ci-cargo-llvm-cov-show-env.md). When the nightly preview lands, evaluate whether its cache-key design generalizes to "instrumented vs not"; that would obsolete the show-env workaround.
- **`LLVM_PROFILE_FILE` defaulting in `.cargo/config.toml`** — tracked in [concerns/profraw-stray-files-outside-target.md](concerns/profraw-stray-files-outside-target.md). The workspace defaults this env var so stray instrumented runs (lingering binaries in `target/debug/deps/` from past coverage builds) write under `target/profraw/` instead of leaking `default_<hash>_<pid>.profraw` to per-crate cwds. Cargo-team posture is "user-side responsibility"; revisit if [rust-lang/cargo#13040](https://github.com/rust-lang/cargo/issues/13040) or [#13136](https://github.com/rust-lang/cargo/issues/13136) progress.
- **Switching to nightly Rust** — tracked in [ideas/ci-nightly-build-knobs.md](ideas/ci-nightly-build-knobs.md). Worth re-opening if CI cycle time becomes a real bottleneck for PR turnaround.

## Re-running the build-time bench

[`scripts/bench_build_baselines.py`](../scripts/bench_build_baselines.py) is a re-runnable harness for cold/incremental/CI build-time measurement. Drops `target/debug` and `target/llvm-cov-target` between cold steps, measures each step's wall time, captures `cargo --timings` HTML reports. Output lands in gitignored `bench-results/baseline/`; rename the dir to `before-<change>` / `after-<change>` to diff. Useful when picking up any of the deferred ideas above.

Run inside the Nix dev shell:

```bash
nix shell nixpkgs#python3 --command python3 scripts/bench_build_baselines.py
```

## References

- [`packaging/licensing/deny.toml`](../packaging/licensing/deny.toml) — `[graph]` / `[licenses]` / `[bans]` config with per-entry rationale.
- [`crates/tisvlay-gui/build.rs`](../crates/tisvlay-gui/build.rs) — slint-build.
- [`crates/tisvlay-gui-app/build.rs`](../crates/tisvlay-gui-app/build.rs) — env-var emitter, cargo-about wrapper with stub fallback.
- [`justfile`](../justfile) — `test-ci` recipe (show-env entry point), `deny` recipe.
- [Cargo cross-workspace-cache 2026 goal](https://rust-lang.github.io/rust-project-goals/2026/cargo-cross-workspace-cache.html) — upstream work that may obsolete the share-target workaround.
- [taiki-e/cargo-llvm-cov `show-env`](https://github.com/taiki-e/cargo-llvm-cov?tab=readme-ov-file#get-coverage-of-cc-code-linked-to-rust-libtest-binary) — the `--sh` invocation our pipeline depends on.
- Sibling docs: [testing.md](testing.md), [licensing.md](licensing.md), [diagnostics.md](diagnostics.md), [design.md](design.md).
