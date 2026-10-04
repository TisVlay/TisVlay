# Code quality: lints, tools and dependency hygiene

**Status:** draft for review, 2026-10-04. Nothing in here is adopted yet. The open questions in §7 decide the final shape; the plan is written so each PR stands alone and can be dropped without breaking the others.

## 1. Goal

Make the compiler and a few cheap tools refuse the classes of mistake this codebase can actually ship: a panic in the daemon, an unchecked length from a TV or a file, a broken doc link, an unused or duplicated dependency, a feature combination that no longer builds. Do it without importing lint policy that fights the code we have, and without a lint table so noisy that `#[allow]` becomes reflex.

The trigger was a 2026 "Rust toolkit" note proposing `pedantic` and `nursery` at deny plus a dozen restriction lints (`unwrap_used`, `expect_used`, `indexing_slicing`, `arithmetic_side_effects`, `as_conversions`, `panic`, …). §3 measures that proposal against TisVlay and against what well-regarded Rust projects do in late 2026; it is adopted in intent, not verbatim.

## 2. Where we are (measured 2026-10-04 on `main` @ `7ac0fdb9`)

**Lint configuration.**

- `[workspace.lints]` holds three lints: `unreachable_pub` and `missing_debug_implementations` (rust, warn) and `undocumented_unsafe_blocks` (clippy, warn). All 17 crates inherit it (`[lints] workspace = true`); no crate carries crate-root lint attributes.
- `just clippy` passes `-- -D warnings`, so every `warn` is already a hard gate. There is no `clippy.toml` and no `rustfmt.toml` (edition 2024 already selects `style_edition = 2024` through cargo).
- The Slint UI crate (`tisvlay-gui`) wraps `include_modules!()` in a crate-root `#![allow(unreachable_pub, missing_debug_implementations, missing_docs)]` with a comment instead of a `reason`.

**Probe.** One `cargo clippy --workspace --all-targets --all-features` run with `pedantic`, `nursery`, the note's restriction lints and a few candidates at warn, a temporary `clippy.toml` setting every `allow-*-in-tests` key, results de-duplicated per site and `tests/` files excluded:

- **149,854 warnings in Slint-generated code** (the `OUT_DIR` module). Slint's generator already allows `clippy::all`, `pedantic` and `nursery` in its output, but not restriction lints or rustc's allow-by-default lints, so every restriction lint adopted needs an allow list on the UI crate's include.
- **2,997 distinct sites in our own production code.** By lint: `missing_docs_in_private_items` 643 · `missing_docs` 544 · `redundant_pub_crate` 214 · `doc_markdown` 206 · `must_use_candidate` 198 · `missing_errors_doc` 140 · `missing_const_for_fn` 87 · `indexing_slicing` 84 · `arithmetic_side_effects` 62 · `needless_pass_by_value` 60 · `as_conversions` 60 · `print_stdout` 55 · `too_long_first_doc_paragraph` 41 · `unused_qualifications` 39 · `expect_used` 33 · `redundant_clone` 31 · `map_unwrap_or` 31 · `allow_attributes_without_reason` 28 · `semicolon_if_nothing_returned` 21 · `redundant_closure_for_method_calls` 21 · `uninlined_format_args` 17 · `use_self` 15 · `cast_lossless` 10 · `unwrap_used` 8 · `string_slice` 7 · `cast_possible_truncation` 6 · `manual_let_else` 5 · `unimplemented` 4 · `panic` 3 · `exit` 2 · `unreachable` 1.
- **The panic family is small and specific.** `unwrap_used`: `tisvlay-session/src/screencast.rs` (5), `tisvlay-lg/src/protocol.rs:322`, `tisvlay-daemon/src/netlink.rs:142`, `tisvlay-gui/build.rs`. `unimplemented!`: four sites in `tisvlay-world/src/reactor.rs`. `panic!`: three in `tisvlay-test` (test infrastructure). `exit`: the tray and the CLI. `print_stdout`: all 55 in the CLI, which prints by design.
- **`missing_docs` per crate:** world 146, SDK (`tisvlay`) 123, wire 116, tray 52, lg 37, daemon 26, session 24, core 15, cec 2. The five `publish = true` crates (core, lg, cec, ssdp, mock) total **54** items.
- **External-input crates** (where a length or index comes from a TV, the network or a file): `indexing_slicing` / `arithmetic_side_effects` / `string_slice` / `cast_possible_truncation` are a few dozen sites across ssdp, lg, cec and the daemon's EDID probe.

**Unsafe.** 26 `unsafe` blocks in six files: libc ACL calls in `tisvlay-core/src/access.rs` (13) and `tisvlay-daemon/src/acl.rs` (5), one each in the GUI's runtime probe, session's logind and screencast modules, and the test D-Bus fakes. No `unsafe_code` policy exists; `undocumented_unsafe_blocks` is the only guard.

**Rustdoc.** Never gated. `cargo doc --workspace --no-deps --all-features` reports 26 warnings (10 broken intra-doc links, 16 public docs linking private items) plus an output-filename collision between the SDK's `tisvlay` library and the CLI's `tisvlay` binary.

**Dependencies.**

- `cargo-shear` (seconds, no build) reports 13 unused or misplaced dependencies, most real: `insta` and `rstest` in core, `insta` in lg, the workspace's `insta`, `insta-cmd` and `indexmap`, three `tempfile` dev-deps left behind by the temp-path work, `tracing-subscriber` in the GUI app, `tokio` misplaced in `tisvlay-test`, and `pipewire-native-spa` (probably a false positive: a sibling crate of a git pin).
- `[package.metadata.cargo-machete]` exists in the GUI app, but nothing runs machete; it finds 1 of the 13.
- `cargo deny` `[bans] multiple-versions = "warn"` with a reasoned skip list, documented in `docs/build.md` § Dep-graph posture. Because it only warns, it drifted: **19 duplicated crates are not on the skip list.** One is ours (`tisvlay-gui-app` asks for `gettext-rs 0.8` where Slint uses `0.7.7`, so both build, each with its own `gettext-sys`); `toml` is ours too (1.x vs zenoh-config's 0.9) but would need a downgrade; the other 17 are upstream-gated (`syn` 2/3, `rand`, `digest`, `nix`, `spin`, `itertools`, `libloading`, `miniz_oxide`, `winnow`, …).
- **RustSec advisories are checked nowhere.** `just deny` runs `licenses bans sources`; advisories live in the weekly `audit.yml` workflow, and GitHub Actions is disabled on the repo.

**Other.** `typos` reports 42 hits, nearly all false positives (`Pn` in UPnP prose, `DELET` in a key example, the truncated `.sunshine-wrapp` process name); the real ones are a handful (`mis` in `help.slint`, `inh` in a test, the `unparseable` spelling). Every gate builds with `--all-features`, so a crate that no longer builds with a feature off goes unnoticed (core's `access`, `wayland`, `logging`; cec's `test-link`). The `rust-version = "1.95"` floor is verified by hand only (`docs/ideas/msrv-verification-job.md`).

## 3. What the ecosystem does (survey, 2026-10-04)

Configs read from each project's default branch on 2026-10-04 (dates are the last commit touching the file); feature status from the cargo and clippy changelogs and checked on our own toolchains where noted.

| Project | Groups | Notable picks |
|---|---|---|
| Bevy | defaults | `allow_attributes` + `allow_attributes_without_reason`, `undocumented_unsafe_blocks`, `print_stdout`/`print_stderr`, `manual_let_else`, `semicolon_if_nothing_returned`, `doc_markdown`; rust `unsafe_code = "deny"`, `missing_docs`, `unused_qualifications` |
| ruff, uv (Astral) | `pedantic` warn at priority −2, ~20 pedantic lints allowed (`must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `too_many_lines`, `similar_names`, `map_unwrap_or`, `if_not_else`, …) | `print_stdout`/`print_stderr`, `dbg_macro`, `exit`, `get_unwrap`, `rc_buffer`, `rc_mutex`; nursery `redundant_clone`; rust `unreachable_pub`, `unsafe_code` warn |
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
- **`redundant_pub_crate` (nursery) contradicts `unreachable_pub`:** every project that picked one picked `unreachable_pub`. The Embark shared lint list is archived (last commit 2025-01) and should not be used as a template.

**Feature status that shapes the plan.**

- **`#[expect(lint, reason = "…")]`** has been stable since 1.81. It is the ratchet: when the last site goes, `unfulfilled_lint_expectations` fires.
- **`build.warnings` is stable since Cargo 1.97** (2026-07). `CARGO_BUILD_WARNINGS=deny` denies only local packages' lint warnings without invalidating the build cache, and the Clippy book now recommends it over `-D warnings`. Verified on our 1.99: no `-Z` needed.
- **`[lints.cargo]` stabilizes in Cargo 1.100** (due 2026-11-12). It brings `unused_dependencies`, `unused_workspace_dependencies` and `missing_lints_inheritance`. Verified: nightly 1.101 honors `unused_dependencies` without an opt-in flag, while stable 1.99 ignores the table.
- **Lint-table inheritance plus a per-crate override is still impossible** (cargo#13157, open). A crate deviates with crate-root attributes or a full table copy.
- **`clippy.toml` has per-lint test exemptions:** `allow-{unwrap,expect,panic,indexing-slicing,dbg,print}-in-tests`. They cover `#[test]` fns and `#[cfg(test)]` modules only, not helpers under `tests/`. The generic `allow-in-tests = [...]` (rust-clippy#17520) is unmerged.
- **Pedantic membership changes every release**, which our pinned toolchain absorbs. Examples: `uninlined_format_args` moved back to pedantic in 1.90; `collapsible_else_if` moved style → pedantic in 1.94.

**Tools** (versions from crates.io and the pinned nixpkgs):

- **cargo-shear** (1.14, 2026-09) replaced cargo-machete as the unused-dependency tool. It parses with rust-analyzer's parser rather than regexes, and also catches misplaced dev/normal deps.
- **typos-cli** (1.50) is the de facto spell checker for code; set `locale = "en-us"` to enforce American English.
- **cargo-hack** (0.6.45) checks each feature on its own and feature powersets.
- **cargo-semver-checks** already runs through release-plz for the published crates.
- **cargo-mutants** (27.1) is mature; it fits a scoped, manual or labeled run, not a gate.
- **Nightly-only formatting options** (`imports_granularity`, `group_imports`, comment wrapping) are still unstable: stay on stable rustfmt.
- **Tombi** is overtaking taplo as the TOML formatter; taplo has had no release since 2025-05.
- **Not worth it for this codebase:**
  - Miri, Kani, loom and cargo-geiger: our `unsafe` is FFI that Miri can't execute.
  - cargo-vet and cargo-crev: their review burden doesn't fit one maintainer.
  - cargo-udeps: needs nightly.
  - `unused_crate_dependencies`: fires once per compile target, so it's noisy.

## 4. The rules this plan follows

1. **Pick lints, never groups** — except possibly `pedantic` with an allow list (open question §7.3). No `nursery`, `restriction` or `clippy::cargo` group, ever; nursery and restriction lints enter one at a time on their merits.
2. **`warn` means deny.** Gates run with warnings denied, so a new lint enters at `warn` (= blocking) and any crate not yet clean carries a root `#![expect(clippy::…, reason = "ratchet: code-quality")]` that must go when its last site does. The plan's target is **zero ratchets at the end of each PR** (open question §7.4).
3. **Every allow states its reason.** `#[expect(…, reason = "…")]` where the lint fires unconditionally, `#[allow(…, reason = "…")]` where it is configuration-dependent (feature- or test-only), a crate-root attribute where a crate must differ from the table.
4. **Generated code is not ours to lint.** The UI crate's include carries one allow list naming every lint the table will ever hold, with one reason, so no later lint has to revisit it.
5. **`unreachable_pub` stays; `redundant_pub_crate` never enters.**
6. **Stable toolchain only.** No nightly rustfmt options, no `-Z` flags in `.cargo/config.toml` (NixOS users build the package with nixpkgs' stable rustc). Gates that need another toolchain (the MSRV floor) get their own recipe.
7. **Each new lint is proven red** on a deliberate violation before its PR is reviewed.

## 5. The PRs

Each PR passes `just check` and `prek run -a` and lists its before → after counts.

### Q1 — Gates and the free denies (no behavior change)

1. **`build.warnings` for the gates.** `just clippy`, `just test-ci` and `just doc` set `CARGO_BUILD_WARNINGS=deny` instead of passing `-- -D warnings`, so plain `cargo build`/`cargo test` builds and rustdoc warnings are covered too, without invalidating the cache. Not set in `.cargo/config.toml`, so a local `cargo run` with an unused variable still builds (open question §7.1).
2. **`just doc`**: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`, in `just check`. Fix the 26 warnings; set `doc = false` on the CLI's `[[bin]]` to end the filename collision with the SDK library.
3. **Free denies** (≤ a handful of sites each): clippy `dbg_macro`, `todo`, `unimplemented` (the four `reactor.rs` sites get a reason or a real implementation), `exit` (the tray and CLI sites get `#[expect(reason)]`), `get_unwrap`, `rc_buffer`, `rc_mutex`, `empty_drop`; rust `unused_qualifications` (39, auto-fixable), `unsafe_op_in_unsafe_fn`, `non_ascii_idents`, `trivial_numeric_casts`.
4. **`allow_attributes_without_reason` + `allow_attributes`**: the ~28 bare `#[allow]`s become reasoned `#[expect]`s (an unfulfilled one is a stale allow, deleted) or reasoned `#[allow]`s where the lint is configuration-dependent. Check empirically that the lints don't fire on Slint's own reason-less allows inside the `include!`d module (expected not, since the generator wraps its items in its own module, but unverified).
5. **The UI crate's allow list** gains the restriction lints this plan adopts, with one `reason = "generated by slint-build"`.
6. **`unsafe_code = "deny"`** in the table; each of the six files with `unsafe` gets a module- or crate-level `#![allow(unsafe_code, reason = "libc ACL FFI")]`-style exception, so a new `unsafe` anywhere else is refused. `multiple_unsafe_ops_per_block` (2 sites) rides along.
7. **`clippy.toml`**: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`, `allow-dbg-in-tests`, `allow-print-in-tests`; plus a short `disallowed-methods` list with reasons. One candidate is `std::process::exit` outside `main`; others are blocking std I/O calls that our async crates should reach through tokio (to be enumerated, open question §7.6).

### Q2 — Tools and dependency hygiene

1. **cargo-shear** replaces the dead machete metadata. `just shear` with `--deny-warnings`, in `just check` and as a prek hook on `Cargo.toml` changes. The 13 findings are verified one by one and removed, or listed under `[package.metadata.cargo-shear] ignored` with the reason. When the toolchain pin reaches 1.100, re-evaluate replacing it with `[workspace.lints.cargo] unused_dependencies` (ships with cargo, no extra tool).
2. **typos**: `just typos` in `just check` and as a prek hook; `_typos.toml` with `locale = "en-us"` (the American-English rule, enforced), `extend-words` for the protocol tokens (`Pn`, `DELET`, `wrapp`, `EIS`) and excludes for `.pot`/`.po`, `Cargo.lock` and `target/`. Fix the few real typos.
3. **Duplicate versions**: align `gettext-rs` to Slint's `0.7` in the GUI app; add a reasoned `skip` for each of the other 18 (`skip-tree` with a small depth only for the winit → SCTK 0.19 subtree the docs already describe); then flip `multiple-versions` to `"deny"` so a dependency bump that adds a duplicate fails `just deny` instead of drifting. Turn on cargo-deny's `unnecessary-skip` reporting so stale skips are flagged. Update `docs/build.md` § Dep-graph posture.
4. **Advisories**: a `just audit` recipe (`cargo deny check advisories`, network needed, so not in the offline `just check`) and a line in the release checklist; re-enabled CI keeps the weekly workflow.
5. **Feature combinations**: `just hack` = `cargo hack check --each-feature --no-dev-deps` on the crates that have features (core, cec, daemon, …), a manual and pre-release recipe rather than part of `just check` (cost to be measured).
6. **The MSRV floor**: `just msrv` builds `cargo check --workspace --all-features` with rust-overlay's stable `1.95` toolchain on `PATH` inside the devShell, and `CARGO_BUILD_WARNINGS` unset. That's the recipe `docs/ideas/msrv-verification-job.md` already documents; this makes it a recipe and retires the idea.

### Q3 — Panic policy in production code

1. **`unwrap_used`** at warn (blocking): the 8 production sites become `expect("… because …")` stating the invariant, or real error handling where an input can actually be malformed. `screencast.rs`' five and `protocol.rs:322` are the ones to read carefully.
2. **`expect_used` stays out.** An `expect` with a reason is the stated-invariant idiom; 33 sites keep it.
3. **`panic`** at warn, with the three `tisvlay-test` sites allowed (test infrastructure) and the CLI's `unreachable!` given a reason.
4. **`print_stdout`/`print_stderr`** at the library crates' roots (`#![warn]` cannot be expressed per crate in the shared table, so a crate-root attribute in each library); the CLI, tray and daemon binaries print freely. Deno's reason applies: `println!` panics on a closed pipe.

### Q4 — The pedantic and style picks

**Hand-picked (default proposal).** These are auto-fixable or near it, and in line with Bevy, rustls and the kernel:

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

**Alternative: the Astral model** (open question §7.3).
- **The setting:** `pedantic = { level = "warn", priority = -1 }`, with ruff/uv's allow list as the starting point (`must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `too_many_lines`, `similar_names`, `map_unwrap_or`, `if_not_else`, `module_name_repetitions`, `struct_excessive_bools`, `used_underscore_binding`, …).
- **What remains:** about 450 sites, 206 of them `doc_markdown`.
- **What it buys:** every new pedantic lint arrives automatically on a toolchain bump.
- **What it costs:** that same churn, plus a larger first sweep.

**Never:** `needless_pass_by_value` (60; it fights `move`-into-task signatures), `missing_const_for_fn` (nursery; its suggestions can be wrong), `must_use_candidate` (very noisy; `#[must_use]` goes by hand on handles whose drop cancels something, e.g. zenoh tokens and subscribers, with `return_self_not_must_use` for builders as an optional pick).

### Q5 — The external-input profile

Crate- or module-root `#![warn]` of `indexing_slicing`, `arithmetic_side_effects`, `string_slice` and `cast_possible_truncation` only where bytes arrive from outside:

- the SSDP reply parser (`tisvlay-ssdp`);
- the SSAP message decoding (`tisvlay-lg` protocol/client);
- CEC frame handling (`tisvlay-cec`);
- the daemon's EDID/sysfs probe;
- the netlink attribute parsing.

That's a few dozen sites. Each becomes a checked access, a stated bound in an `#[expect(reason)]`, or a typed refusal. Anything that reads like a real bug is written up first, fixed red-first, and never silently changed in the sweep.

### Q6 — Docs on the published crates

`missing_docs` at warn on the five `publish = true` crates (core, lg, cec, ssdp, mock), 54 items. Every doc says something a reader needs; filler (`/// The client.`) is rejected in review. The unpublished crates (world, SDK, wire, …, about 490 items) stay out of scope: their consumers are in-tree.

### Later, measured first or waiting on upstream

- **`[workspace.lints.cargo]`** once the pin moves to 1.100: `unused_dependencies` (possibly retiring cargo-shear) and `missing_lints_inheritance`.
- **cargo-mutants**, scoped to pure-logic crates (core, wire, the world's reducers, the parsers), run manually or on a PR label, never a gate.
- **Fuzzing the parsers** (SSDP, SSAP JSON, EDID, CEC) with bolero, so one harness runs as a property test on stable and as a fuzz target on nightly.
- **turmoil** for the LG reconnect/backoff logic: deterministic network simulation, vendor layer only (zenoh won't run under it).
- **Tombi** for TOML formatting: one large reformatting diff over comment-heavy manifests, so it only lands if the result reads well.
- **crates.io trusted publishing** for the release-plz job, once the five crates' first publish is done by hand.
- **`clippy.toml` `allow-in-tests`** when rust-clippy#17520 merges, replacing the per-lint keys.

### Never adopt (recorded so nobody re-scouts)

- **Groups:** `nursery`, `restriction`, `clippy::cargo`. `multiple_crate_versions` walks the whole graph per crate, and cargo-deny already covers duplicates.
- **Restriction lints workspace-wide:** `expect_used`, `indexing_slicing`, `arithmetic_side_effects` and `as_conversions`. None of the surveyed projects uses them.
- **Conflicting or broken lints:** `redundant_pub_crate`; `missing_docs_in_private_items`.
- **Nightly-dependent options:** rustfmt's nightly-only options, a nightly toolchain pin (measured 2026-10-04: `-Zthreads` saved 3–5 s on cold builds and nothing on incremental ones), and cargo-udeps.
- **Tools:** Miri, Kani, loom, cargo-geiger, cargo-vet, cargo-crev, rust-code-analysis (stale), and `unused_crate_dependencies`.

## 6. Order and size

Q1 and Q2 are independent of each other and of everything below, and are mostly mechanical. Q3 and Q5 touch behavior on malformed input and want careful review. Q4 is mechanical (`clippy --fix`) once its shape is chosen. Q6 is writing. Expected order: Q1 → Q2 → Q3 → Q4 → Q5 → Q6, each its own branch and PR. Q4 can run in parallel with Q3 since the fixes rarely overlap.

## 7. Open questions

1. **`build.warnings`: gates only, or also local builds?** Setting `[build] warnings = "deny"` in `.cargo/config.toml` makes every local `cargo build` fail on a warning, the strictest option and the most annoying mid-edit. The draft sets it only in the gate recipes.
2. **Keep `-D warnings` as the mechanism, or move to `CARGO_BUILD_WARNINGS`?** The draft moves. It also covers `cargo test` builds and doesn't need `--` plumbing.
3. **Pedantic: hand-picked (about 140 sites) or Astral-style group minus an allow list (about 450, mostly `doc_markdown`)?**
4. **To zero in each PR, or ratchet with crate-root `#![expect]` and clean up over time?** At about 3,000 sites, mostly docs we wouldn't adopt anyway, zero per PR looks feasible for everything in Q1–Q5.
5. **`missing_docs` scope:** the five published crates only, or also the SDK (`tisvlay`) and `tisvlay-wire`, whose types are the IPC contract other tools might code against?
6. **`disallowed-methods`:** worth curating a list (blocking std I/O in async crates, `process::exit` outside `main`, direct `tokio::signal` use outside the shutdown helper), or skip?
7. **Typos in identifiers:** fix misspelled public names (a breaking change for the published crates), or exclude identifiers and check prose only?
8. **`just hack` and `just msrv` in `just check`**, or manual/pre-release only? The draft keeps both out of `just check` until their cost is measured.
9. **Tombi:** try it in a throwaway branch to judge the diff, or skip TOML formatting entirely?
