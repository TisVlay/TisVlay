# Code quality: lints, tools and dependency hygiene

**Status:** decided, 2026-10-04. The plan was revised after a design, a mechanics and a maintainability review, and the owner then ruled on every open question (§7). Nothing is implemented yet. Each PR stands alone and can be dropped without breaking the others. §8 says where every rule lives once this file retires.

## 1. Goal

Make the compiler and a few cheap tools refuse the classes of mistake this codebase can actually ship: a panic in the daemon, an unchecked length from a TV or a file, a broken doc link, an unused or duplicated dependency, a feature combination that no longer builds, a secret in a log line. Do it without importing lint policy that fights the code we have, and without a lint table so noisy that `#[allow]` becomes reflex.

The trigger was a 2026 "Rust toolkit" note proposing `pedantic` and `nursery` at deny plus a dozen restriction lints (`unwrap_used`, `expect_used`, `indexing_slicing`, `arithmetic_side_effects`, `as_conversions`, `panic`, …). §3 measures that proposal against TisVlay and against what well-regarded Rust projects do in late 2026; it is adopted in intent, not verbatim.

## 2. Where we are (measured 2026-10-04 on `main` @ `7ac0fdb9`)

**Lint configuration.**

- `[workspace.lints]` holds three lints: `unreachable_pub` and `missing_debug_implementations` (rust, warn) and `undocumented_unsafe_blocks` (clippy, warn). All 17 crates inherit it (`[lints] workspace = true`).
- Crate-root lint attributes exist in two places only:
  - `tisvlay-gui`'s `#![allow(unreachable_pub, missing_debug_implementations, missing_docs)]` around Slint's `include_modules!()`, justified by a comment rather than a `reason`;
  - an `#![allow(dead_code)]` in `tisvlay-world/tests/support/mod.rs`.
- `just clippy` passes `-- -D warnings`, so every `warn` is already a hard gate. There is no `clippy.toml` and no `rustfmt.toml`; edition 2024 already selects `style_edition = 2024` through cargo.

**Probe.** One `cargo clippy --workspace --all-targets --all-features` run with `pedantic`, `nursery`, the note's restriction lints and a few candidates at warn. A temporary `clippy.toml` set every `allow-*-in-tests` key, and results were de-duplicated per site.

- **Generated code: 149,854 warnings in Slint's `OUT_DIR` module.**
  - Slint's generator allows `clippy::all`, `pedantic` and `nursery` in its output, but not restriction lints or rustc's allow-by-default lints.
  - Among the hits: `trivial_numeric_casts` 15,019, `unused_qualifications` 3,555, `unwrap_used` 2,246, `panic` 863, `todo` 174.
  - `allow_attributes*` do not fire there.
- **Production code: 2,997 distinct sites** (crate `src/` outside `#[cfg(test)]`). By lint:
  - docs: `missing_docs_in_private_items` 643 · `missing_docs` 544 · `doc_markdown` 206 · `missing_errors_doc` 140 · `too_long_first_doc_paragraph` 41;
  - pedantic/nursery noise: `redundant_pub_crate` 214 · `must_use_candidate` 198 · `missing_const_for_fn` 87 · `needless_pass_by_value` 60 · `map_unwrap_or` 31;
  - restriction: `indexing_slicing` 84 · `arithmetic_side_effects` 62 · `as_conversions` 60 · `print_stdout` 55 · `expect_used` 33 · `allow_attributes_without_reason` 28 · `unwrap_used` 8 · `string_slice` 7 · `cast_possible_truncation` 6 · `exit` 2 · `unreachable` 1;
  - picks: `unused_qualifications` 39 · `redundant_clone` 31 · `semicolon_if_nothing_returned` 21 · `redundant_closure_for_method_calls` 21 · `uninlined_format_args` 17 · `use_self` 15 · `cast_lossless` 10 · `manual_let_else` 5.
- **Production panics are few and specific.**
  - `unwrap_used`: `tisvlay-session/src/screencast.rs` (5), `tisvlay-lg/src/protocol.rs:322`, `tisvlay-daemon/src/netlink.rs:142`, `tisvlay-gui/build.rs`.
  - `panic!`: three in `tisvlay-test`, which is test infrastructure.
  - `exit`: the tray and the CLI.
  - `print_stdout`: all 55 in the CLI, which prints by design. `print_stderr`: none outside tests.
  - The probe's four `unimplemented!` (`tisvlay-world/src/reactor.rs`) and five `get_unwrap` (`tisvlay-core/src/config.rs`) sit inside `#[cfg(test)]` modules. They're test code with no matching `allow-*-in-tests` key.
- **Integration tests are outside the per-lint test keys.** Those keys cover `#[test]` fns and `#[cfg(test)]` modules, not helper fns under `tests/`. `crates/*/tests/` holds 291 `.unwrap()` and 25 `panic!` calls across 25 files, and `docs/testing.md` requires a diagnostic panic in every bounded-drain helper.
- **`missing_docs` per crate:** world 146, SDK (`tisvlay`) 123, wire 116, tray 52, lg 37, daemon 26, session 24, core 15, cec 2. The five `publish = true` crates (core, lg, cec, ssdp, mock) total **54** items.
- **External-input code** (a length or index from a TV, the network or a file): `indexing_slicing`, `arithmetic_side_effects`, `string_slice` and `cast_possible_truncation` hit a few dozen sites across ssdp, lg, cec and the daemon's EDID probe.

**Unsafe.** 26 `unsafe` blocks in eight files:

- libc ACL calls: `tisvlay-core/src/access.rs` (13) and `tisvlay-daemon/src/acl.rs` (5);
- one each in the GUI's runtime probe, session's `logind.rs` and `screencast.rs`, and `tisvlay-daemon/tests/access.rs`;
- the test D-Bus fakes: `tisvlay-test/src/dbus.rs` and `fake_logind.rs`.

No macro expansion (Slint, zbus, tokio) trips `unsafe_code`. No `unsafe_code` policy exists; `undocumented_unsafe_blocks` is the only guard.

**Rustdoc.** Never gated. `cargo doc --workspace --no-deps --all-features` reports 26 warnings: 10 broken intra-doc links and 16 public docs linking private items, in core 9, mock 6, lg 5, cec 4, icons 1 and world 1. It also reports an output-filename collision between the SDK's `tisvlay` library and the CLI's `tisvlay` binary.

**Dependencies.**

- **cargo-shear** (seconds, no build) reports 13 unused or misplaced dependencies, and all 13 check out:
  - `insta` and `rstest` in core, `insta` in lg;
  - the workspace's `insta`, `insta-cmd` and `indexmap`;
  - three `tempfile` dev-deps (SDK, tray, wire);
  - `tracing-subscriber` in the GUI app;
  - `tokio` misplaced in `tisvlay-test`;
  - `pipewire-native-spa` (unused directly; `pipewire-native` pulls it in);
  - after `--fix`, the workspace checks green. `--fix` drops a `features = ["test-util"]` it rewrites, so fixes go in by hand.
- **The `cargo-machete` metadata is dead config:** `[package.metadata.cargo-machete]` exists in the GUI app, nothing runs machete, and machete finds 1 of the 13.
- **The insta removal exposes stale docs.** No `insta::` use or `snapshots/` directory exists, yet `docs/testing.md`, `CLAUDE.md` and the justfile's `test-snapshot-*` recipes describe a snapshot layer. `proptest` is a listed layer with one `proptest!` in the tree, and `.config/nextest.toml` says "retry up to 2 times" beside `retries = 0`.
- **Duplicate versions are policed by a warning only.** `cargo deny` runs `[bans] multiple-versions = "warn"` with a reasoned skip list, documented in `docs/build.md` § Dep-graph posture. Because it only warns, it drifted: **19 duplicated crates are on no skip list.**
  - **One is ours:** `tisvlay-gui-app` asks for `gettext-rs 0.8` where Slint uses `0.7.7`. They share one `gettext-sys`, and 0.7 has the same `gettext`/`pgettext` API we use.
  - **`toml` is ours too** (1.x vs zenoh-config's 0.9), but fixing it would mean a downgrade.
  - **The other 17 are upstream-gated:** `syn` 2/3, `rand`, `rand_core`, `digest`, `crypto-common`, `block-buffer`, `cpufeatures`, `nix` (×3), `spin`, `itertools`, `libloading`, `miniz_oxide`, `winnow`, `socket2`, `dirs`, `downcast-rs`, `toml_datetime`.
  - cargo-deny's `unnecessary-skip` and `unmatched-skip` are always-on warnings, not config keys. The current skips trip neither.
- **RustSec advisories are checked nowhere.** `just deny` runs `licenses bans sources`; advisories live in the weekly `audit.yml` workflow, and GitHub Actions is disabled.

**Other.**

- **`typos`:** 42 hits without a locale, nearly all false positives: UPnP split at its lowercase letters, a pluralized all-caps zenoh verb, the truncated Sunshine process name. The real ones were a handful (a hyphenated "misattribute", a test fixture address, one spelling of "unparsable"). With `locale = "en-us"` it adds about 60 British spellings, most of them in the ARP neighbor-table code.
- **Feature combinations are never checked.** Every gate builds with `--all-features`, so a crate that no longer builds with a feature off goes unnoticed. The affected features are core's `access`, `wayland` and `logging`, and cec's `test-link`. `docs/build.md` § CI feature scope argues for exactly one feature configuration.
- **The MSRV floor is manual.** `rust-version = "1.95"` is verified by hand only, following `docs/ideas/msrv-verification-job.md`.
- **The nextest `ci` profile retries once**, which hides flaky tests.
- **Secrets can reach logs.** `Credentials` (it holds the LG client key) derives `Debug`. No current log line prints it, but `missing_debug_implementations` pushes `Debug` everywhere, and `#[instrument]` or `?state` would leak it into journald.
- **The published `tisvlay-core` exposes anyhow in its public API** (`logging::init`, the `access` feature).

## 3. What the ecosystem does (survey, 2026-10-04)

Configs read from each project's default branch on 2026-10-04 (dates are the last commit touching the file); feature status from the cargo and clippy changelogs and checked on our own toolchains where noted.

| Project | Groups | Notable picks |
|---|---|---|
| Bevy | defaults | `allow_attributes` + `allow_attributes_without_reason`, `undocumented_unsafe_blocks`, `print_stdout`/`print_stderr`, `manual_let_else`, `semicolon_if_nothing_returned`, `doc_markdown`; rust `unsafe_code = "deny"`, `missing_docs`, `unused_qualifications` |
| ruff, uv (Astral) | `pedantic` warn at priority −2, ~20 pedantic lints allowed (`must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `too_many_lines`, `similar_names`, `map_unwrap_or`, `if_not_else`, …) | `print_stdout`/`print_stderr`, `dbg_macro`, `exit`, `get_unwrap`; nursery `redundant_clone`; rust `unreachable_pub`, `unsafe_code` warn |
| rust-analyzer | correctness/perf deny, others warn, restriction allow | `dbg_macro`, `todo`, `print_*` (deny in CI); every allow carries a one-line justification |
| rustls | defaults | `manual_let_else`, `redundant_clone`, `use_self`, `cloned_instead_of_copied`; rust `unreachable_pub`, `unnameable_types`, `unused_qualifications` |
| wasmtime, cargo | `all = "allow"`, then opt back in | `allow_attributes_without_reason` (wasmtime), `print_*`, `dbg_macro`, `disallowed_methods` backed by `clippy.toml` (cargo) |
| Deno | defaults, CLI flags | `allow_attributes_without_reason`, `print_*` (std print panics on a broken pipe), `unused_async`, `large_futures`; per-crate `disallowed-methods` |
| Zed | `style` allowed | `dbg_macro`, `todo`, `redundant_clone`, `disallowed_methods`; custom lints via Dylint |
| tokio, Linux kernel | crate attributes / Makefile flags | `missing_docs`, `unreachable_pub`, `unsafe_op_in_unsafe_fn`; the kernel adds `cast_lossless`, `ptr_as_ptr`, `ignored_unit_patterns` |

**The consensus.**

- **No surveyed project enables `restriction`, `nursery` or `clippy::cargo` as groups.** The Clippy book says `restriction` "should, *emphatically*, not be enabled as a whole".
- **Only Astral enables `pedantic` wholesale**, at warn, with an allow list.
- **None of `unwrap_used`, `expect_used`, `indexing_slicing`, `arithmetic_side_effects`, `as_conversions` appears in any of them.**
- **The lints that recur:** `print_stdout`/`print_stderr`, `dbg_macro`/`todo`, `allow_attributes_without_reason`, `undocumented_unsafe_blocks`, `unreachable_pub`, `redundant_clone`, `manual_let_else`, `use_self`, and `disallowed_methods` as a project-specific guard rail.
- **`redundant_pub_crate` (nursery) contradicts `unreachable_pub`:** every project that picked one picked `unreachable_pub`.
- **The Embark shared lint list is archived** (last commit 2025-01) and should not be used as a template.

**Feature status that shapes the plan** (each item checked on our toolchains unless marked).

- **`#[expect(lint, reason = "…")]`** has been stable since 1.81. When the last site goes, `unfulfilled_lint_expectations` fires.
- **`build.warnings`** has been stable since Cargo 1.97.
  - `CARGO_BUILD_WARNINGS=deny` fails clippy, test and rustdoc builds on local packages' warnings.
  - Toggling it doesn't invalidate the cache, unlike `-- -D warnings`, and cached warnings are replayed.
  - It doesn't cover build-script `cargo::warning=` lines or cargo-level warnings (manifest keys, filename collisions).
  - It works under `cargo llvm-cov show-env`, and cargo 1.95 silently ignores it.
  - The flake's nixpkgs rustc (1.98.1) honors it, and `buildRustPackage` reads `.cargo/config.toml`.
- **`[lints.cargo]` stabilizes in Cargo 1.100.**
  - It brings `unused_dependencies`, `unused_workspace_dependencies` and `missing_lints_inheritance`.
  - Nightly 1.101 honors `unused_dependencies` without an opt-in flag. Stable 1.99 warns "unused manifest key", which `build.warnings` doesn't catch.
- **Lint-table inheritance plus a per-crate override is still impossible** (cargo#13157, open). A crate deviates through crate-root attributes or a full table copy.
- **`clippy.toml`'s per-lint test keys** (`allow-{unwrap,expect,panic,indexing-slicing,dbg,print}-in-tests`) all exist in 1.99. They cover `#[test]` fns and `#[cfg(test)]` modules only. The generic `allow-in-tests = [...]` (rust-clippy#17520) is unmerged.
- **`clippy::allow_attributes` fires on every outer `#[allow]`, reasoned or not**, including inside `cfg_attr`. Only inner `#![allow]` escapes it.
- **Pedantic membership changes between releases** (group-move details unverified). A pinned toolchain only delays that churn to the next bump; it doesn't absorb it.

**Tools** (versions from crates.io and the pinned nixpkgs):

- **cargo-shear** (nixpkgs 1.13.4) replaced cargo-machete as the unused-dependency tool: it parses with rust-analyzer's parser and catches misplaced dev/normal deps.
- **typos-cli** (1.50) is the de facto spell checker for code; `locale = "en-us"` enforces American English.
- **cargo-hack** (0.6.45) checks each feature in isolation.
- **cargo-semver-checks** already runs through release-plz for the published crates.
- **cargo-mutants** (27.1) is mature; it fits a scoped, manual or labeled run.
- **Formatting:** nightly-only rustfmt options remain unstable. Taplo's maintainer stepped away in December 2024 and it hasn't released since May 2025; Tombi is replacing it in some projects (wgpu, prettier-plugin-toml), though Taplo configs still outnumber Tombi's about ten to one.
- **Not worth it here:**
  - Miri, Kani, loom and cargo-geiger: our `unsafe` is FFI that Miri can't execute.
  - cargo-vet and cargo-crev: their review burden doesn't fit one maintainer.
  - cargo-udeps: needs nightly.
  - `unused_crate_dependencies`: fires once per compile target, so it's noisy.

## 4. The rules this plan follows

These are the durable part. §8 moves them into `docs/build.md` and `CLAUDE.md`.

1. **Pick lints, never groups.** No `pedantic`, `nursery`, `restriction` or `clippy::cargo` group is *enabled*. Their lints enter one at a time on their merits. Allowing a group, as on generated code, is fine.
2. **`warn` means deny.** The gates deny warnings, so a new lint enters at `warn` and is blocking from day one. Each PR takes its lints to zero sites; no crate-root ratchet survives a PR. Where a site must stay, it carries a reasoned `#[expect]`.
3. **Every allowance states its reason.**
   - Use `#[expect(…, reason = "…")]` where the lint fires unconditionally.
   - Use `#[cfg_attr(<cond>, expect(…, reason = "…"))]` or a module-level `#![allow(…, reason = "…")]` where it depends on features or tests.
   - `clippy::allow_attributes_without_reason` enforces this. `clippy::allow_attributes` is not adopted: it would flag the reasoned allows this rule depends on.
4. **Strict by default, exceptions opt out.** A lint goes in the workspace table so every new crate inherits it. A crate that legitimately differs, such as the CLI printing to stdout, opts out with one reasoned crate-root attribute. A lint isn't spread across crates as per-crate opt-ins.
5. **Generated code is not ours to lint.** The Slint include allows whole groups plus `warnings`, rather than a lint-by-lint list, so a lint added later never has to revisit it:
   ```rust
   #![allow(
       warnings,
       clippy::restriction,
       clippy::pedantic,
       clippy::nursery,
       reason = "generated by slint-build",
   )]
   ```
6. **`unreachable_pub` stays; `redundant_pub_crate` never enters.**
7. **Stable toolchain only.**
   - No nightly rustfmt options.
   - No `-Z` flags and no `[build] warnings` in `.cargo/config.toml`: NixOS users build the package with nixpkgs' stable rustc, which reads that file, so a newer compiler's new warning would break their rebuild.
   - Gates that need another toolchain, like the MSRV floor, get their own recipe.
8. **Each new lint is proven red** on a deliberate violation before its PR is reviewed.
9. **Every tool that joins `just check` comes from the flake devShell** and gets a prek hook whose id matches its recipe name, with a file filter where it's cheap to have one.

## 5. The PRs

Each PR passes `just check` and `prek run -a`, lists its before → after counts, and updates every doc or comment it contradicts (named per PR below).

### Q1a — The gates

1. **`build.warnings` in the gate recipes.**
   - `just clippy`, `just test-ci` and `just doc` export `CARGO_BUILD_WARNINGS=deny` instead of `-- -D warnings`.
   - This also covers the test and rustdoc builds, and toggling it doesn't invalidate the cache.
   - Local `cargo build` / `cargo run` stay warn-only. That's the gates-only answer to §7.1, and §4.7 explains why the setting never goes in `.cargo/config.toml`.
   - Update the justfile's `-D warnings` comments, `docs/testing.md`'s mention, and `CLAUDE.md`'s `just clippy` line.
2. **`just doc`** runs `cargo doc --workspace --no-deps --all-features` under `CARGO_BUILD_WARNINGS=deny`, without a separate `RUSTDOCFLAGS`, which would only add a second fingerprint.
   - It joins `just check` and gets a prek hook filtered on `\.rs$|Cargo\.toml`.
   - Fix the 26 warnings.
   - Set `doc = false` on the CLI's `[[bin]]`, which `build.warnings` doesn't cover, to end the filename collision.
   - Measured cost: 37 s cold, nothing when nothing changed, and 2 s after editing a crate.

### Q2 — Tools and dependency hygiene

Each tool joins the flake devShell in this PR, together with its first use: cargo-shear, typos, cargo-hack, and tombi if adopted.

1. **cargo-shear replaces the dead machete metadata.**
   - `just shear` runs `cargo shear --deny-warnings`. It joins `just check`, with a prek hook filtered on `Cargo\.toml$`.
   - The 13 findings are fixed by hand, not with `--fix`.
   - Remove the insta-related doc drift: the `test-snapshot-*` recipes, `docs/testing.md`'s snapshot layer, `CLAUDE.md`'s pointer, and the stale nextest retry comment.
   - Only cargo-shear's `ignored` list depends on review to stay honest; record whether shear warns on stale entries.
2. **typos.**
   - `just typos` joins `just check`, with a local prek hook dispatching to it, so the version comes from the flake.
   - `_typos.toml` sets `locale = "en-us"` and allow-lists only names this repo can't rename: netlink-packet-route's British-spelled neighbor types (`NeighbourState` and friends), `oif`, the portal's EIS, the truncated Sunshine comm, and UPnP-derived product names. It excludes `.pot`/`.po`, `Cargo.lock` and `target/`.
   - Fix the real typos, misspelled public identifiers included, since nothing is published yet (§7).
3. **Duplicate versions become blocking.**
   - `gettext-rs` stays on 0.8 (reversed from the first draft). Aligning to Slint's 0.7 put our own crate directly on RUSTSEC-2026-0244, the unsound `setlocale`, which 0.8 fixes by marking it `unsafe`. Slint's 0.7.7 gets a skip like the other upstream pins instead.
   - Give each of the other 18 duplicates an exact `name@version` skip, on the *older* version, with a reason naming the upstream gate. The skip then goes stale, and fails, once upstream catches up.
   - Flip `multiple-versions` to `"deny"`.
   - Run `just deny` with `-D unnecessary-skip -D unmatched-skip` so a stale skip is an error too.
   - Collapse the winit → SCTK 0.19 entries into one `skip-tree` only if it reads better; it removes none of the 18.
   - **The bump routine** goes in `docs/build.md`. A dependency update that adds a duplicate fails `just deny`, so: skip the old version with its reason, or wait. The emergency escape is a one-off `cargo deny check bans -W multiple-versions`, never a config flip.
   - Update `docs/build.md` § Dep-graph posture, the `deny` recipe comment, the `deny.toml` header and `docs/licensing.md`. All of them say bans never fail.
4. **Advisories:** a `just audit` recipe running `cargo deny check advisories`. It needs the network, so it stays out of the offline `just check`.
5. **Feature combinations:** a `just hack` recipe running `cargo hack check --each-feature --no-dev-deps --workspace`. Update `docs/build.md` § CI feature scope, which argues for a single configuration and already omits core's `access`.
6. **The MSRV floor:** a `just msrv` recipe running `cargo check --workspace --all-targets --all-features --target-dir target/msrv` with rust-overlay's stable `1.95` toolchain on `PATH` inside the devShell.
   - No `--ignore-rust-version`: the toolchain *is* the floor, so leaving the flag off keeps transitive MSRV creep visible.
   - `docs/ideas/msrv-verification-job.md` retires: the local recipe and the CI job below cover both of its options.
7. **CI runs the slow checks, as is customary.**
   - GitHub Actions is re-enabled.
   - `msrv` and `hack` become pull-request jobs, run in parallel so their minutes don't matter. CI provisions toolchains with rustup, so the MSRV job installs `1.95` directly; the rustup traps are specific to the NixOS devShell.
   - `audit` runs weekly (the existing `audit.yml`) and on `Cargo.lock` changes.
   - The `just` recipes exist to reproduce a CI failure locally.
   - `just check` stays fast and offline.
8. **A Tombi trial — done 2026-10-04, not adopted yet.** On the nixpkgs build (Tombi 1.5.2), `tombi format` rewrote 23 of 28 TOML files (394+/365−). It indented by two spaces, sorted package fields and dependency tables, and scattered the comment-grouped dependency sections. With `indent-width = 4` and the schema turned off, sorting stopped, but it still regrouped tables: the daemon's `[dependencies]` and `[lints]` moved below `[package.metadata.nfpm]`. The knobs that disable reordering (`table-keys-order`, `array-values-order`) are documented for 1.7 and don't parse in 1.5.2. Revisit when nixpkgs ships Tombi ≥ 1.7 (a Later item).
   - Taplo's author stepped away in December 2024 (taplo#715). Its last release was in May 2025, and it has about 240 open issues.
   - Tombi releases weekly. wgpu (June 2026) and prettier-plugin-toml (September 2026) have switched to it.

### Q1b — The free denies and the unsafe policy

1. **Workspace-table lints:**
   - clippy `dbg_macro`, `todo`, `unimplemented`, `exit`, `get_unwrap` and `allow_attributes_without_reason`;
   - rust `unused_qualifications` (39 sites, auto-fixable) and `non_ascii_idents`;
   - the four test-module `unimplemented!` become `unreachable!` or real fakes, and the five `get_unwrap` become `[]` or `expect`;
   - the tray's `exit` gets a reasoned `#[expect]` (`clippy::exit` already exempts `main`);
   - the ~28 bare `#[allow]`s become reasoned `#[expect]`s (an unfulfilled one is a stale allow, deleted) or reasoned `cfg_attr`/inner allows.
2. **`print_stdout` and `print_stderr` in the table**, with one crate-root `#![allow(…, reason = "the CLI prints its output")]` in `tisvlay-cli`. Deno's reason applies: `println!` panics on a closed pipe, and the daemons log through tracing.
3. **The generated-code allow** of §4.5 replaces `tisvlay-gui`'s current list.
4. **`unsafe_code = "deny"`** in the table. Each of the eight files gets a module- or file-level `#![expect(unsafe_code, reason = "…")]`, so an exception whose `unsafe` disappears fires `unfulfilled_lint_expectations`. `deny`, not `forbid`, because `forbid` would reject those exceptions. `multiple_unsafe_ops_per_block` (2 sites) rides along.
5. **`clippy.toml`:** `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`, `allow-dbg-in-tests` and `allow-print-in-tests`.
6. **The conventions section in `CLAUDE.md`** starts here and grows with each later PR. It's a short list that steers new code before clippy has to:
   - `expect("… because …")` states an invariant;
   - thiserror errors in libraries, anyhow only in binaries;
   - tracing fields rather than format arguments;
   - no prints outside the CLI;
   - secrets redacted in `Debug`;
   - reasoned `#[expect]` over `#[allow]`.
7. **Dropped from the first draft:**
   - `unsafe_op_in_unsafe_fn` already warns in edition 2024.
   - `rc_buffer` and `rc_mutex` have zero sites in an Arc/tokio codebase.
   - `trivial_numeric_casts` overlaps the default `unnecessary_cast`.
   - No `disallowed-methods` list until a real incident motivates an entry (§7). An entry for `process::exit` would duplicate `clippy::exit` anyway.

### QT — One integration-test binary per crate, and the flaky-test policy

Clippy treats `#[test]` fns and `#[cfg(test)]` modules as tests, but not helper fns under `tests/`. So Q3's `unwrap_used` and `panic` would flag the 291 unwraps and 25 diagnostic panics in those helpers. Restructuring the tests fixes that and pays for itself.

1. **Consolidate.** Each crate's `tests/*.rs` binaries become one `tests/it/main.rs` with modules. That's 25 binaries today: daemon 7, world 4, `tisvlay-test` 3, tray and gui-app 2 each, the rest 1.
   - Shared helpers become ordinary modules, which retires world's `#![allow(dead_code)]` on `tests/support`.
   - Measured before: a daemon edit relinks seven test binaries (2.4 s on 32 cores, linked in parallel), and those seven take 791 MB on disk. The PR measures the after.
   - Test names gain a module prefix (`it::wire::…`), so update recipes and docs that filter by test binary name. nextest still runs every test in its own process, and the hermetic-test work already made plain `cargo test` safe with many tests in one binary.
2. **One allow per crate.** Each `tests/it/main.rs` root carries a reasoned `#![allow(clippy::unwrap_used, clippy::panic, reason = "integration tests fail loudly by design")]`. That's about ten lines in all, deleted when rust-clippy#17520's `allow-in-tests` lands.
3. **Flaky tests are quarantined, never retried.** The nextest `ci` profile's retries go from 1 to 0. A flaky test gets an issue and `#[ignore = "flaky: #N"]` until fixed. The policy goes into `docs/testing.md`.

### Q3 — Malformed input and panics

The first draft split this into a panic PR and an external-input PR. It's one review: what happens when input is wrong.

1. **`unwrap_used` in the table.**
   - The 8 production sites become `expect("… because …")` stating the invariant, or real error handling where the input can actually be malformed. `screencast.rs`' five and `protocol.rs:322` need the careful read.
   - Integration tests are covered by QT's per-crate allow.
   - `expect_used` stays out: an `expect` with a reason is the stated-invariant idiom.
2. **`panic` in the table.** The three `tisvlay-test` sites get reasoned `#[expect]`s, since `tisvlay-test` is test infrastructure. The integration-test helpers are covered by QT.
3. **The external-input profile.** A module-root `#![warn]` of `indexing_slicing`, `arithmetic_side_effects`, `string_slice` and `cast_possible_truncation` where bytes arrive from outside:
   - the SSDP reply parser;
   - the SSAP message decoding in lg;
   - CEC frame handling;
   - the daemon's EDID/sysfs probe;
   - the netlink attribute parsing.

   Each site becomes a checked access, a stated bound in an `#[expect(reason)]`, or a typed refusal. Anything that reads like a real bug is written up, fixed red-first, and never silently changed in the sweep.
4. **A "never panics on arbitrary input" property test per parser**, with the `proptest` already in the tree, on stable. Running the code on adversarial input proves more than the lint does.
5. **Credentials get a redacting `Debug`**, hand-written for `Credentials` and anything else holding the LG client key, with a test that the key never appears in the `Debug` output.

### Q6 — The published crates: core's glue moves out, docs, and the first-publish checklist

1. **Binary glue leaves the published `tisvlay-core`.**
   - A new unpublished crate (working name `tisvlay-runtime`) takes the `logging` module (journald/stderr tracing setup) and the `access` module (pkexec socket-group enrollment), with their features and dependencies: `tracing-subscriber`, `tracing-journald`, `tracing-log`, `libc` and `anyhow`.
   - The binaries that call them (CLI, daemon, session, tray, greeter, GUI app) switch to it.
   - The published `core` then keeps only what vendor backends need, and anyhow leaves its API (C-GOOD-ERR).
   - Evaluate the `wayland` and `mutter-idle` idle transports for the same move. They're desktop glue too, but typed, so they move only if that reads cleaner.
   - This lands before `missing_docs`, so the new crate isn't documented to published-crate standard for nothing.
2. **`missing_docs` on the five `publish = true` crates** (core, lg, cec, ssdp, mock), through a crate-root `#![warn(missing_docs)]` in each. That's 54 items today, fewer once core's glue moves.
   - Every doc says something a reader needs; filler (`/// The client.`) is rejected in review.
   - The unpublished crates stay out.
3. **A first-publish checklist in `docs/build.md`**, run once per crate before its hand-done first publish:
   - `cargo package` (catches path-only deps, a missing readme or license, a bloated `include`);
   - `[package.metadata.docs.rs] all-features = true` where features gate items;
   - the crate-root `missing_docs` added when `publish = true` flips;
   - no `anyhow` or other binary-glue type in the public API.

### Q4 — The pedantic and style picks

These go last because they matter least. Hand-picked lints, in line with Bevy, rustls and the kernel:

| Lint | Group | Sites |
|---|---|---|
| `redundant_clone` | nursery, individually | 31 |
| `semicolon_if_nothing_returned` | pedantic | 21 |
| `redundant_closure_for_method_calls` | pedantic | 21 |
| `uninlined_format_args` | pedantic | 17 |
| `use_self` | nursery, individually | 15 |
| `cast_lossless` | pedantic | 10 |
| `ptr_as_ptr` / `borrow_as_ptr` | pedantic | 9 / 8 |
| `manual_let_else` | pedantic | 5 |
| `ignored_unit_patterns` | pedantic | 5 |

That's about 140 sites, mostly `cargo clippy --fix`.
- **Named task: `#[must_use]` by hand** on the handles whose drop cancels something (zenoh tokens, subscribers, the liveliness guards). There are zero `#[must_use]`s today. `return_self_not_must_use` covers the builders.
- **A yearly pedantic probe replaces the Astral model.** Once or twice a year, rerun the §2 probe with `pedantic` at warn and consider new picks; it goes in `docs/build.md` § Review triggers. The rejected alternative, `pedantic` at warn minus an allow list, would cost about 450 sites now (206 of them `doc_markdown`) and a sweep on every toolchain bump.
- **Never in this PR:**
  - `needless_pass_by_value` (60; it fights `move`-into-task signatures);
  - `missing_const_for_fn` (nursery; its suggestions can be wrong);
  - `must_use_candidate` (very noisy).

### Later, measured first or waiting on upstream

Each becomes a `docs/ideas/*.md` entry when the plan retires (§8).

- **`[workspace.lints.cargo]`** once the pin reaches the release that stabilizes it: `unused_dependencies` (possibly retiring cargo-shear) and `missing_lints_inheritance`.
- **cargo-mutants**, scoped to pure-logic crates (core, wire, the world's reducers, the parsers), run manually or on a label, never a gate.
- **Fuzzing the parsers with bolero**, extending Q3's property tests into fuzz targets on nightly.
- **turmoil** for the LG reconnect/backoff logic. Zenoh won't run under it, so it covers the vendor layer only.
- **Tombi** once nixpkgs ships ≥ 1.7, whose ordering toggles should leave the comment-grouped manifests intact (Q2.8's trial).
- **crates.io trusted publishing** for the release-plz job, after each crate's first hand publish.
- **`clippy.toml` `allow-in-tests`** when rust-clippy#17520 merges, replacing the per-lint keys and most test-file allows.

### Never adopt (recorded so nobody re-scouts)

- **Groups:** `nursery`, `restriction` and `clippy::cargo` as groups. `multiple_crate_versions` walks the whole graph per crate, and cargo-deny covers duplicates.
- **Restriction lints workspace-wide:** `expect_used`, `indexing_slicing`, `arithmetic_side_effects` and `as_conversions`. No surveyed project uses them.
- **Conflicting or noisy lints:** `allow_attributes` (it conflicts with rule 3), `redundant_pub_crate`, `missing_docs_in_private_items`.
- **Nightly-dependent options:** rustfmt's nightly-only options, a nightly toolchain pin (measured 2026-10-04: `-Zthreads` saved 3–5 s on cold builds and nothing on incremental ones), and cargo-udeps.
- **Tools:** Miri, Kani, loom, cargo-geiger, cargo-vet, cargo-crev, rust-code-analysis (stale), and `unused_crate_dependencies`.

## 6. Order and size

**Order:** Q1a → Q2 → Q1b → QT → Q3 → Q6 → Q4, each its own branch and PR.

- **Q1a and Q2** are mechanical gate and tool work. They come first because they make every later PR's gates real.
- **Q1b** is lint additions with small site counts.
- **QT** restructures the tests so Q3's lints land on code that should obey them.
- **Q3** changes behavior on malformed input and wants the careful review.
- **Q6** is a crate split plus writing.
- **Q4** is cosmetic churn and goes last.

**Sequencing:** no open feature PR conflicts with the workspace-wide sweeps. The feature branches merged as #115, #116, #118 and #121, and the only open PR (#81, the mdBook docs) touches four crate files. So the PRs land in plan order and #81 rebases.

## 7. Decisions (ruled 2026-10-04)

1. **`build.warnings` only in the gate recipes** (clippy, test-ci, doc), never in `.cargo/config.toml`, so NixOS rebuilds can't break when a newer compiler adds a warning (§4.7).
2. **Integration tests: one test binary per crate** with one reasoned allow per crate root (QT), rather than an allow in each of the 25 files or lints on `src/` only.
3. **Pedantic: hand-picked lints, plus a pedantic probe once or twice a year** to consider new picks (Q4). The Astral group model was rejected.
4. **Each PR takes its lints to zero.** A site that must stay carries a reasoned `#[expect]`; no crate-level ratchet markers.
5. **`missing_docs` on the published crates only**, and on any crate when it flips to `publish = true`.
6. **No `disallowed-methods` list** until a real incident motivates an entry.
7. **Misspelled identifiers are fixed now**, public ones included, while nothing is published.
8. **The slow checks run in CI, as is customary.** GitHub Actions is re-enabled: `msrv` and `hack` as pull-request jobs, `audit` weekly and on `Cargo.lock` changes. The `just` recipes reproduce them locally (Q2.7).
9. **`tisvlay-core`'s binary glue (`logging`, `access`) moves to a new unpublished crate**, so the published core carries no anyhow and fewer dependencies (Q6.1).
10. **Tombi: a throwaway trial first**, adopted in Q2 only if the reformat reads well (Q2.8).
11. **Sequencing: plan order.** The feature branches the draft worried about have merged.
12. **Flaky tests are quarantined, never retried**, and the nextest `ci` profile's retries go to 0 (QT.3).
13. **A conventions section in `CLAUDE.md`**, started in Q1b and grown with each PR.
14. **`doc`, `shear` and `typos` join `just check`**, each with a matching prek hook: `doc` filtered on `\.rs$|Cargo\.toml`, `shear` on `Cargo\.toml$`.

## 8. Where this lands

Per `docs/plans/README.md`, this file is deleted when its last PR merges. Each PR moves its part:

- **`docs/build.md` gains a "Lint policy" section**, holding the rules of §4, the never-adopt list, and a short survey summary under a dated "Lessons" heading.
  - The bump routine for duplicates goes into § Dep-graph posture.
  - The first-publish checklist gets its own section, and so do the CI jobs with their local recipes.
  - The time-bound items go under § Review triggers, phrased as triggers rather than dates:
    - shear vs `[lints.cargo]` when the pin reaches that release;
    - the per-lint keys and the QT test allows when rust-clippy#17520 merges;
    - the yearly pedantic probe.
- **`docs/testing.md`** gets the one-binary-per-crate layout and the flaky-test quarantine policy, and loses the stale snapshot layer.
- **`CLAUDE.md` § Linting & CI** gets a three-line summary of the rules plus the conventions section (§7.13). The "Dead Code Detection" note stays, since shear finds unused dependencies, not unused `pub` items. The crate tree gains the new runtime crate.
- **`Cargo.toml`** gets a comment above `[workspace.lints]` pointing to the lint policy. That's where someone adding a lint will look.
- **`docs/ideas/`** gets one entry per "Later" item, with index lines in `docs/ideas.md`. `msrv-verification-job.md` and its index line retire, since Q2's recipe and CI job cover both of its options.
- **Version-anchored facts stay in this plan and in git history**, not in the durable docs: tool versions, release numbers, dates, the probe's counts.
