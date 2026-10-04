# Code quality: lints, tools and dependency hygiene

**Status:** draft, revised after a design, a mechanics and a maintainability review (2026-10-04). Nothing in here is adopted yet. The open questions in §7 decide the final shape; each PR stands alone and can be dropped without breaking the others. §8 says where every rule lives once this file retires.

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

- **`typos`:** 42 hits, nearly all false positives (`Pn` in UPnP prose, `DELET` in a key example, the truncated `.sunshine-wrapp` process name). The real ones are a handful: `mis` in `help.slint`, `inh` in a test, the `unparseable` spelling.
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
- **Formatting:** nightly-only rustfmt options remain unstable. Tombi is overtaking taplo, which has had no release since 2025-05.
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
3. **The tools in the flake devShell:** cargo-shear, typos and cargo-hack.

### Q2 — Tools and dependency hygiene

1. **cargo-shear replaces the dead machete metadata.**
   - `just shear` runs `cargo shear --deny-warnings`. It joins `just check`, with a prek hook filtered on `Cargo\.toml$`.
   - The 13 findings are fixed by hand, not with `--fix`.
   - Remove the insta-related doc drift: the `test-snapshot-*` recipes, `docs/testing.md`'s snapshot layer, `CLAUDE.md`'s pointer, and the stale nextest retry comment.
   - Only cargo-shear's `ignored` list depends on review to stay honest; record whether shear warns on stale entries.
2. **typos.**
   - `just typos` joins `just check`, with a local prek hook dispatching to it, so the version comes from the flake.
   - `_typos.toml` sets `locale = "en-us"`, adds `extend-words` for the protocol tokens (`Pn`, `DELET`, `wrapp`, `EIS`), and excludes `.pot`/`.po`, `Cargo.lock` and `target/`.
   - Fix the real typos, misspelled identifiers included (§7.7).
3. **Duplicate versions become blocking.**
   - Align `gettext-rs` to Slint's `0.7`.
   - Give each of the other 18 duplicates an exact `name@version` skip, on the *older* version, with a reason naming the upstream gate. The skip then goes stale, and fails, once upstream catches up.
   - Flip `multiple-versions` to `"deny"`.
   - Run `just deny` with `-D unnecessary-skip -D unmatched-skip` so a stale skip is an error too.
   - Collapse the winit → SCTK 0.19 entries into one `skip-tree` only if it reads better; it removes none of the 18.
   - **The bump routine** goes in `docs/build.md`. A dependency update that adds a duplicate fails `just deny`, so: skip the old version with its reason, or wait. The emergency escape is a one-off `cargo deny check bans -W multiple-versions`, never a config flip.
   - Update `docs/build.md` § Dep-graph posture, the `deny` recipe comment, the `deny.toml` header and `docs/licensing.md`. All of them say bans never fail.
4. **Advisories:** a `just audit` recipe running `cargo deny check advisories`, plus where it runs (§7.8). It needs the network, so it stays out of the offline `just check`.
5. **Feature combinations:** a `just hack` recipe running `cargo hack check --each-feature --no-dev-deps --workspace`. Update `docs/build.md` § CI feature scope, which argues for a single configuration and already omits core's `access`.
6. **The MSRV floor:** a `just msrv` recipe running `cargo check --workspace --all-targets --all-features --target-dir target/msrv` with rust-overlay's stable `1.95` toolchain on `PATH` inside the devShell.
   - No `--ignore-rust-version`: the toolchain *is* the floor, so leaving the flag off keeps transitive MSRV creep visible.
   - `docs/ideas/msrv-verification-job.md` keeps only its CI-job options, with the local recipe now real.

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
6. **Dropped from the first draft:**
   - `unsafe_op_in_unsafe_fn` already warns in edition 2024.
   - `rc_buffer` and `rc_mutex` have zero sites in an Arc/tokio codebase.
   - `trivial_numeric_casts` overlaps the default `unnecessary_cast`.
   - A `disallowed-methods` entry for `process::exit` would duplicate `clippy::exit`.

### Q3 — Malformed input and panics

The first draft split this into a panic PR and an external-input PR. It's one review: what happens when input is wrong.

1. **`unwrap_used` in the table.**
   - The 8 production sites become `expect("… because …")` stating the invariant, or real error handling where the input can actually be malformed. `screencast.rs`' five and `protocol.rs:322` need the careful read.
   - Integration-test files follow §7.2.
   - `expect_used` stays out: an `expect` with a reason is the stated-invariant idiom.
2. **`panic` in the table.** The three `tisvlay-test` sites and the integration-test helpers are handled per §7.2.
3. **The external-input profile.** A module-root `#![warn]` of `indexing_slicing`, `arithmetic_side_effects`, `string_slice` and `cast_possible_truncation` where bytes arrive from outside:
   - the SSDP reply parser;
   - the SSAP message decoding in lg;
   - CEC frame handling;
   - the daemon's EDID/sysfs probe;
   - the netlink attribute parsing.

   Each site becomes a checked access, a stated bound in an `#[expect(reason)]`, or a typed refusal. Anything that reads like a real bug is written up, fixed red-first, and never silently changed in the sweep.
4. **A "never panics on arbitrary input" property test per parser**, with the `proptest` already in the tree, on stable. Running the code on adversarial input proves more than the lint does.
5. **Credentials get a redacting `Debug`**, hand-written for `Credentials` and anything else holding the LG client key, with a test that the key never appears in the `Debug` output.

### Q6 — Docs on the published crates, and the first-publish checklist

1. **`missing_docs` on the five `publish = true` crates** (core, lg, cec, ssdp, mock), 54 items, through a crate-root `#![warn(missing_docs)]` in each.
   - Every doc says something a reader needs; filler (`/// The client.`) is rejected in review.
   - The unpublished crates stay out, unless §7.5 says otherwise.
2. **A first-publish checklist in `docs/build.md`**, run once per crate before its hand-done first publish:
   - `cargo package` (catches path-only deps, a missing readme or license, a bloated `include`);
   - `[package.metadata.docs.rs] all-features = true` where features gate items;
   - the crate-root `missing_docs` added when `publish = true` flips;
   - the anyhow-in-public-API decision (§7.9).

### Q4 — The pedantic and style picks

These go last: they matter least and conflict most with open branches (§6). Hand-picked lints, in line with Bevy, rustls and the kernel:

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
- **The Astral model** (pedantic at warn minus an allow list) is the alternative, at about 450 sites, 206 of them `doc_markdown`, plus a sweep on every toolchain bump (§7.3).
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
- **Tombi** for TOML formatting, only if its reformat of the comment-heavy manifests reads well (§7.10).
- **crates.io trusted publishing** for the release-plz job, after each crate's first hand publish.
- **`clippy.toml` `allow-in-tests`** when rust-clippy#17520 merges, replacing the per-lint keys and most test-file allows.

### Never adopt (recorded so nobody re-scouts)

- **Groups:** `nursery`, `restriction` and `clippy::cargo` as groups. `multiple_crate_versions` walks the whole graph per crate, and cargo-deny covers duplicates.
- **Restriction lints workspace-wide:** `expect_used`, `indexing_slicing`, `arithmetic_side_effects` and `as_conversions`. No surveyed project uses them.
- **Conflicting or noisy lints:** `allow_attributes` (it conflicts with rule 3), `redundant_pub_crate`, `missing_docs_in_private_items`.
- **Nightly-dependent options:** rustfmt's nightly-only options, a nightly toolchain pin (measured 2026-10-04: `-Zthreads` saved 3–5 s on cold builds and nothing on incremental ones), and cargo-udeps.
- **Tools:** Miri, Kani, loom, cargo-geiger, cargo-vet, cargo-crev, rust-code-analysis (stale), and `unused_crate_dependencies`.

## 6. Order and size

**Order:** Q1a → Q2 → Q1b → Q3 → Q6 → Q4, each its own branch and PR.

- **Q1a and Q2** are mechanical gate and tool work. They come first because they make every later PR's gates real.
- **Q1b** is lint additions with small site counts.
- **Q3** changes behavior on malformed input and wants the careful review.
- **Q6** is writing.
- **Q4** is cosmetic churn and goes last.

**Sequencing rule:** workspace-wide auto-fix sweeps conflict with every open feature branch. That's `unused_qualifications` in Q1b and the `clippy --fix` passes in Q4. They land after the open feature PRs merge, or immediately before a quiet stretch (§7.11).

## 7. Open questions

1. **`build.warnings`: gates only (draft), or also local builds?** The draft keeps it to the gate recipes. `.cargo/config.toml` is ruled out either way (§4.7).
2. **Integration-test files under `unwrap_used` and `panic`:** what applies to their 291 unwraps and 25 diagnostic panics?
   - One `#![allow(clippy::unwrap_used, clippy::panic, reason = "integration test")]` per `tests/*.rs` file (25 files) until rust-clippy#17520 lands?
   - Or keep both lints out of the table and apply them only to the daemon and library `src/`?
3. **Pedantic: hand-picked (draft, about 140 sites) or Astral-style (about 450 sites, plus a sweep on every toolchain bump)?** All three reviews recommend hand-picked for a single maintainer.
4. **Zero per PR (draft) or crate-root ratchets?** At about 3,000 sites, mostly lints we wouldn't adopt anyway, zero per PR looks feasible for Q1–Q4. This is a confirmation question.
5. **`missing_docs` scope:** the five published crates only (draft), or also the SDK and `tisvlay-wire`, whose types are the IPC contract other tools might code against?
6. **`disallowed-methods`:** skip until a real incident motivates an entry (draft, following the design review), or curate a list now?
7. **Misspelled identifiers:** fix them now (draft)? None of the five crates has been published, and the project has no backwards-compatibility rule.
8. **Where do `just hack`, `just msrv` and `just audit` run?** No release checklist exists, and a manual recipe with no trigger never runs. The options:
   - in `just check` (minutes per run, network for `audit`);
   - a written pre-release checklist in `docs/build.md`;
   - re-enabling the CI workflows that would schedule them.
9. **anyhow in `tisvlay-core`'s public API** (`logging::init`, the `access` feature): give those typed `thiserror` errors before first publish, or document them as this repo's binary helpers?
10. **Tombi:** try it on a throwaway branch to judge the diff, or skip TOML formatting entirely?
11. **Sequencing:** hold the workspace-wide sweeps until the open feature branches merge (seat access, CLI completions, the GNOME greeter patch, tray smart scroll, mdBook docs), or land them now and rebase those?
12. **Flaky tests:** adopt "a flaky test gets an issue and `#[ignore = "flaky: #N"]`, never a retry", and set the nextest `ci` profile's retries to 0?
13. **A short conventions section in `CLAUDE.md`?** Much of the code here is written by an agent, which reads `CLAUDE.md` rather than clippy's reasoning. The section would list `expect("… because …")`, the thiserror/anyhow split, tracing fields over format args, no prints outside the CLI, and redacted secrets in `Debug`.
14. **The `just check` time budget:** add `doc`, `shear` and `typos` to `just check` and to prek (draft), at the measured cost in Q1a?

## 8. Where this lands

Per `docs/plans/README.md`, this file is deleted when its last PR merges. Each PR moves its part:

- **`docs/build.md` gains a "Lint policy" section**, holding the rules of §4, the never-adopt list, and a short survey summary under a dated "Lessons" heading.
  - The bump routine for duplicates goes into § Dep-graph posture.
  - The first-publish checklist and the `hack`/`msrv`/`audit` triggers get their own section.
  - The time-bound items go under § Review triggers, phrased as triggers rather than dates: shear vs `[lints.cargo]` when the pin reaches that release, the per-lint keys when rust-clippy#17520 merges, the pedantic decision on a major clippy change.
- **`CLAUDE.md` § Linting & CI** gets a three-line summary of the rules and the conventions section if §7.13 says yes. The "Dead Code Detection" note stays, since shear finds unused dependencies, not unused `pub` items.
- **`Cargo.toml`** gets a comment above `[workspace.lints]` pointing to the lint policy. That's where someone adding a lint will look.
- **`docs/ideas/`** gets one entry per "Later" item, with index lines in `docs/ideas.md`. `msrv-verification-job.md` is trimmed to its CI options.
- **Version-anchored facts stay in this plan and in git history**, not in the durable docs: tool versions, release numbers, dates, the probe's counts.
