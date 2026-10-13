# Build system

Developer-facing reference for how the workspace compiles, what gets pulled into the dep graph, and the conventions that keep the build fast on stable Rust + 2-core CI. Audience: someone touching the test pipeline, adjusting a profile, swapping a dep, or investigating "why does this take so long?". For the testing surface (mock backends, wire tests, observability) see [testing.md](testing.md); for license attribution + policy see [licensing.md](licensing.md); for architectural decisions see [design.md](design.md).

## Build pipeline

`just test-ci` is the canonical CI entry point. It runs the full check pipeline (clippy + nextest + doc tests + coverage report) under a single coverage-instrumented build, so all four stages share `target/llvm-cov-target/` instead of compiling the workspace twice, once plain and once instrumented.

The mechanism: `cargo llvm-cov show-env --sh` emits `export VAR=…` lines that route every later cargo command through cargo-llvm-cov's `RUSTC_WRAPPER`, which adds `-C instrument-coverage` and points `LLVM_PROFILE_FILE` into the target dir. `eval`-ing them in a multi-line bash recipe means clippy, nextest and the doc tests all share one instrumented build. The recipe pins `CARGO_TARGET_DIR=target/llvm-cov-target/` before the `eval`: show-env doesn't choose a directory itself, and cargo doesn't rebuild when only `RUSTC_WRAPPER` changes. Sharing `target/` would therefore reuse an uninstrumented `just build`, leaving no `.profraw` and a failing report.

There's a residual dual-compile that *can't* be eliminated on stable today (verified by `cargo clippy -v` rustc invocations): even with one target dir and one instrumentation setting, cargo treats `cargo clippy` and `cargo nextest run` as different "modes" with different artifact-emit flags. clippy emits `--emit=dep-info,metadata` (rmeta only — interface, no machine code); nextest emits `--emit=dep-info,metadata,link` (rmeta + `.rlib`). cargo's fingerprint distinguishes the two artifact kinds, so every workspace crate is recompiled in the second invocation. This is upstream cargo design — see "Review triggers" for the goal that may eventually fix it.

`tisvlay-gui` looks uniquely doubly-compiled when grepping for `Compiling X` lines because it's the only workspace crate with a `build.rs` — its build-script binary forces a "Compiling" verb in the clippy step where every other workspace crate gets "Checking" (rmeta-only). Cosmetic, not actual extra duplication.

### CI feature scope

CI lints and tests with `--all-features` — the same flags as local `prek` — so the instrumented build has exactly one feature configuration to reason about. The workspace's feature surface is small (runtime's `access`/`logging`/`wayland`/`mutter-idle`, lg's `discovery`, cec's `test-link`; the mock backend is always compiled). What that single configuration can't catch, a crate that no longer builds with one of its features off, is the separate `features` job's concern (`just hack`; see "Checks that run in CI"). The Slint MCP debug server is enabled ad-hoc with `--features slint/mcp` (a dependency feature, not a crate feature) and guards no workspace source.

## Checks that run in CI

Three checks take minutes cold or need the network, so they run as CI jobs rather than in `just check` or prek. Each has a `just` recipe that reproduces it locally:

| Check | CI | Local | What it catches |
|---|---|---|---|
| MSRV floor | `msrv` job, every PR | `just msrv` | Code or a dependency that needs a newer compiler than `rust-version` |
| Feature combinations | `features` job, every PR | `just hack` | A crate that doesn't build with one of its features on its own, or with none |
| Advisories | `audit.yml`, weekly + on `Cargo.lock` or `deny.toml` changes | `just audit` | RustSec advisories and yanked crates |

- **MSRV:** the floor is read from `Cargo.toml` in both places. CI installs it with rustup. Locally, `just msrv` uses the flake's `msrv-rust` toolchain on `PATH` inside the dev shell. rustup-installed toolchains misbehave under the NixOS dev shell: `rustup run` still resolves the shell's own cargo, and they fail on `slint-macros`. There's no `--ignore-rust-version`, because the toolchain is the floor.
- **Raising the floor:** only when an update needs a newer compiler, and never past the rustc that nixpkgs ships. The Nix package builds with the consumer's nixpkgs compiler, and cargo refuses any toolchain older than `rust-version`. Check before raising it: `nix eval --raw --inputs-from . nixpkgs#rustc.version`.
- **Features:** `cargo hack check --each-feature --no-dev-deps`. `--no-dev-deps` strips dev-dependencies from the manifests for the run and restores them after, so rust-analyzer may flag phantom errors while it runs.

## Lint policy

### Warnings are errors in the gates

Every gate recipe denies warnings: `just clippy`, `just doc` and `just test-ci` export `CARGO_BUILD_WARNINGS=deny` (cargo's `build.warnings`). So a lint at `warn` in `[workspace.lints]` blocks a commit and a CI run just as `deny` would.

- **Why `build.warnings` and not `-- -D warnings`:** it covers clippy, test and rustdoc builds with one setting. It also fails on warnings replayed from the cache, and toggling it doesn't invalidate the build cache, which passing `-D warnings` through clippy or `RUSTFLAGS` does. It doesn't cover build-script `cargo::warning=` lines or cargo's own manifest and filename-collision warnings.
- **Why per recipe and not `.cargo/config.toml`:** the Nix package build (`buildRustPackage`) reads that file with nixpkgs' rustc. A warning that a newer compiler adds would then break a NixOS rebuild. Local `cargo build` / `cargo run` stay warn-only, so a half-edited file still runs.
- **Rustdoc is a gate too.** `just doc` builds the workspace's API docs (`--no-deps`), so a broken intra-doc link, or a public doc linking a private item, fails like any other lint. In CI it runs in the Lint step's prek pass. The CLI's binary sets `doc = false`, since it shares the `tisvlay` name with the SDK library.

### Adding a lint

1. **Pick lints, never enable groups.** No `pedantic`, `nursery`, `restriction` or `clippy::cargo` group is enabled; their lints enter the `[workspace.lints]` table one at a time, on their merits. Allowing a group is fine, as on generated code.
2. **Strict by default, exceptions opt out.** A lint goes in the shared table, so every crate (and every new one) inherits it. A crate that legitimately differs opts out with one reasoned crate-root attribute: the CLI's `print_stdout`/`print_stderr`. A lint isn't spread as per-crate opt-ins.
3. **Every allowance states its reason** (`allow_attributes_without_reason`):
   - `#[expect(lint, reason = "…")]` where the lint fires unconditionally. When the last site goes, `unfulfilled_lint_expectations` fails the build, so stale allowances can't linger.
   - `#[allow(lint, reason = "…")]` only where it depends on features or tests, so an `expect` would be unfulfilled in some build. For example, a test-support module shared by binaries that each use a different subset.
4. **A new lint lands at zero sites.** It enters at `warn`, which blocks (see above), and its PR fixes or expects every site; there is no crate-level "fix later" marker.
5. **Prove it red.** Before review, a deliberate violation must fail `just clippy`.
6. **Generated code** (Slint's `include_modules!` in `tisvlay-gui`) sits in a module that allows `warnings` plus the clippy groups, so no lint added later has to revisit it. Deny-level lints such as `unsafe_code` still apply.
7. **`unsafe` is confined.** `unsafe_code` is `deny`, not `forbid`, so the few files that need FFI can opt out with a reasoned file-level `#![expect(unsafe_code)]`. Every block gets a `// SAFETY:` comment (`undocumented_unsafe_blocks`) and does one unsafe operation (`multiple_unsafe_ops_per_block`).
8. **Test code may unwrap, panic, index and print.** `.config/clippy.toml` sets the matching `allow-*-in-tests` keys. They cover `#[test]` fns and `#[cfg(test)]` modules, but not helper fns under `tests/`.
9. **A tool that joins `just check`** comes from the flake devShell (built there from source when nixpkgs lags, as tombi is) and gets a prek hook whose id matches its recipe name, with a file filter where one is cheap.

### Parsers of outside input

Code that handles bytes from outside carries a module-level external-input profile. It's the one deliberate exception to rule 2: these lints are too noisy for the whole workspace (see "Never adopted"), so they're opted into per module rather than out of per crate.
- `#![warn]` of `indexing_slicing`, `arithmetic_side_effects`, `string_slice` and `cast_possible_truncation`;
- applied to SSDP replies, SSAP messages from the TV, CEC frames, EDID and sysfs data, netlink messages, and MAC strings.

Each site is a checked access (`get`, `checked_*`, `saturating_*`), a typed refusal, or a reasoned `#[expect]` stating the bound. Each entry point that parses raw bytes or text (SSDP datagrams, EDID blobs, MAC strings, the TV's power-state payload, CEC input ids and names) also gets a "never panics on arbitrary input" property test (proptest), because the lint finds code that *could* panic and the property test runs it on adversarial input. Modules that only read values a library already decoded (netlink messages, serde-decoded SSAP JSON, the CEC ioctl link) take the profile alone. A new parser of raw input takes both.

### Never adopted

Recorded so nobody re-scouts:

- **Groups:** `nursery` and `restriction` as groups. `clippy::cargo`, whose `multiple_crate_versions` walks the whole graph per crate; cargo-deny's `bans` covers duplicates.
- **Restriction lints workspace-wide:** `expect_used` (an `expect` stating its invariant is the idiom here), `indexing_slicing`, `arithmetic_side_effects` and `as_conversions`. Too noisy for an I/O-bound daemon; the parsers of outside input opt in per module instead.
- **`allow_attributes`:** it flags reasoned `#[allow]`s too, which rule 3 relies on.
- **`redundant_pub_crate`:** it contradicts the `unreachable_pub` lint already in the table.
- **`missing_docs_in_private_items`.**
- **Pedantic lints weighed and passed over**, so the yearly probe needn't re-weigh them: `needless_pass_by_value` (it fights `move`-into-task signatures), `missing_const_for_fn` (its suggestions can be wrong) and `must_use_candidate` (very noisy; `#[must_use]` goes on by hand, see CLAUDE.md's code conventions).
- **A `disallowed-methods` list,** until a real incident motivates an entry. One for `process::exit` would duplicate `clippy::exit`.
- **Nightly-only rustfmt options, and a nightly toolchain:** `-Zthreads` measured at 3–5 s off a cold build and nothing off an incremental one.
- **Tools:** Miri, Kani, loom and cargo-geiger (our `unsafe` is FFI that Miri can't execute); cargo-vet and cargo-crev (their review burden doesn't fit one maintainer); cargo-udeps (needs nightly); rust-code-analysis (stale); `unused_crate_dependencies` (fires once per compile target).
- **Embark's shared lint list as a template:** it's archived.

### Lessons (the October 2026 code-quality pass)

The policy above came from a survey of how well-regarded Rust projects configure lints, measured against this workspace before anything was adopted. The plan and its numbers live in git history.

- **What the ecosystem does.** None of Bevy, ruff/uv, rust-analyzer, rustls, wasmtime, cargo, Deno, Zed, tokio or the Linux kernel enables `restriction`, `nursery` or `clippy::cargo` as a group. Only Astral enables `pedantic`, with an allow list. The lints that recur are the ones adopted here: `print_*`, `dbg_macro`/`todo`, reasoned allows, documented `unsafe`, `unreachable_pub`, `redundant_clone`, `manual_let_else`, `use_self`. None of them uses `unwrap_used`, `indexing_slicing`, `arithmetic_side_effects` or `as_conversions` workspace-wide.
- **Measure before adopting.** A proposal of pedantic and nursery plus a dozen restriction lints, all at deny, measured in the thousands of sites in our code. Most were doc-style lints or contradicted rules already in the table. Slint's generated module added far more, from the restriction lints and rustc's allow-by-default ones: its generator already allows the clippy groups.
- **Converting allows to `#[expect]` is a census.** About a quarter of the bare allows came back unfulfilled: stale, and deleted.
- **A warning-only gate drifts.** With duplicate versions at `warn`, nearly twenty accumulated unnoticed. A gate either blocks or it gets ignored.
- **Tools find real things fast.** cargo-shear found a dozen unused or misplaced dependencies, including a whole snapshot-testing layer the docs described and nothing used. The first advisory run caught an unsound `gettext-rs` that an over-eager dedup had just pulled in. The parser lint profile found no live bug, but turned a couple dozen "safe because of a check elsewhere" sites into code that can't panic.

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

The workspace pulls several hundred transitive crates. Each *version* of a crate is a separate compile unit, so duplicates cost compile time, and on a 2-core CI runner that's real wall time.

The policy is **one version per crate unless an upstream crate forces two.** Each exception is a `skip` entry carrying its reason. A plain warning is not enough: it drifted, letting 19 unexplained duplicates pile up unnoticed.

### `cargo-deny [bans]` config

[`packaging/licensing/deny.toml`](../packaging/licensing/deny.toml) sets `[bans] multiple-versions = "deny"` with a curated `skip` list.
- Each skip entry has a `reason` naming the upstream crate that forces the duplicate, so nobody re-investigates known cases.
- `just deny` runs with `-D unnecessary-skip -D unmatched-skip`, so an entry that no longer matches anything fails too, and the list can't outlive its duplicates.

### The bump routine

A dependency update that introduces a duplicate fails `just deny`, and with it prek and CI. To clear it:

1. **Find the cause:** `cargo tree -d -e normal --target x86_64-unknown-linux-gnu` shows each duplicated crate and who pulls each version.
2. **Dedup if we can.** If one of our own crates asks for the other version, align it.
3. **Otherwise skip the older version exactly**, as `{ crate = "name@x.y.z", reason = "<upstream> pins name x.y; <other> is on z" }`. The exact version makes the entry go stale, and fail, once that upstream catches up, which prompts its removal.
4. **Use `skip-tree` sparingly.** It hides any new duplicates that appear underneath the skipped subtree.

The emergency escape, when a bump must land before its duplicates are triaged, is a one-off `cargo deny --config packaging/licensing/deny.toml check -W duplicate bans`. The config never flips back to `warn`.

The skip list's reasons are the inventory of known duplicates; this doc doesn't repeat them. The big clusters, each worth a re-audit when its upstream bumps:

- **zenoh's pins:** `toml 0.9`, `rand 0.8`, `socket2 0.5`, `nix 0.29`, `itertools 0.14` and the `digest 0.10` crypto stack.
- **winit's SCTK 0.19 path** (through Slint): `rustix 0.38`, `thiserror 1` and `tiny-skia` via `sctk-adwaita`.
- **Slint's compiler internals:** `hashbrown`, through `rowan` and `prost-build`'s `petgraph`.

### `[graph] targets` filter

`[graph] targets = [{ triple = "x86_64-unknown-linux-gnu" }]` restricts cargo-deny's resolution to the platforms we ship for. Without it the default view considers every target triple and surfaces ~20 duplicate warnings for crates we never compile (`objc2-*` Apple, `windows-*` Windows, `jni-sys` Android, `r-efi`, `redox_*`).

`[graph]` is global — it affects `licenses` too. So a Linux-only license-policy gate won't catch an Apple/Windows-conditional one. That trade-off matches shipping reality: every binary we ship is Linux-only (D-Bus + systemd; the GUI is Wayland-only — see [design.md](design.md) "Known Challenges & Mitigations"). If we ever add a non-Linux release artifact, broaden the targets list.

### Default-feature resolution

cargo-deny resolves with default features rather than `--all-features`, matching the dep graph we actually distribute. The MCP debug stack (`i-slint-backend-testing/mcp`'s prost chain, with MPL-2.0 crates we don't audit for production) is reachable only via an ad-hoc `cargo run --features slint/mcp` — a `slint` dependency feature no workspace crate enables — so it's absent from both default- and all-feature resolution and never reaches the licenses gate.

cargo-deny 0.18+ also excludes dev-deps from `bans` and `licenses` by default — no extra `multiple-versions-include-dev` knob needed.

## First-publish checklist

Six crates are `publish = true`: the vendor-backend libraries `tisvlay-core`, `-lg`, `-cec` and `-ssdp`, and the client side, `tisvlay-protocol` (the wire protocol) and `tisvlay` (the client SDK). The mock backend stays unpublished: its API is shaped around the workspace's tests, and new vendors copy it from the repo. release-plz publishes their releases, and its `semver_check` runs cargo-semver-checks from the second release on. Its crates.io token carries only the `publish-update` scope, so it can't create a crate: the first publish of each one is done by hand, and that is when this checklist runs. It also runs whenever a crate flips to `publish = true`.

1. **`just package-check`.** It packages all six crates in one `cargo package` invocation, first as a file list, then as a build of each packaged crate on its own. Read the list for fixtures and stray files (`tisvlay-lg`'s protocol fixtures ship on purpose: its unit tests `include_str!` them). The build catches path-only dependencies, and cargo warns about missing metadata (description, license, documentation, homepage or repository). One invocation matters: cargo resolves the crates' path dependencies on each other through a local overlay, while a per-crate `cargo package -p tisvlay-lg` fails until `tisvlay-core` is on crates.io. The SDK and the protocol exclude their `tests/`, which build against unpublished workspace crates.
2. **A README that is also the crate docs.** Each crate root starts with `#![doc = include_str!("../README.md")]`, so crates.io and docs.rs show the same page and the README's examples run as doctests. A README example that stops compiling against the API fails `just test`. A `LICENSE` symlink to the root license ships the Apache-2.0 text in each package (cargo packages the target file).
3. **`#![warn(missing_docs)]` at the crate root.** Every public item documented, with no filler (`/// The client.` says nothing).
4. **docs.rs sees every public item.** Where a non-default feature gates items, set `[package.metadata.docs.rs] all-features = true` (`tisvlay-lg` does, for `discovery`). Leave it off where the only feature exposes test doubles (`tisvlay-cec`'s `test-link`). The Unix-only crates (`tisvlay-protocol`, `tisvlay`) set `targets` to Linux, since docs.rs builds for other platforms too.
5. **No binary glue in the public API:** no `anyhow`, no logging setup, no desktop transports. Those live in the unpublished `tisvlay-runtime`; a library error is a `thiserror` type the caller can match on.
6. **Publish in dependency order:** `tisvlay-core`, then `tisvlay-ssdp`, then `tisvlay-lg` and `tisvlay-cec`, then `tisvlay-protocol`, then `tisvlay`, or let `cargo publish --workspace` order them. crates.io must already hold a dependency's version before a crate that needs it uploads, optional ones included (`tisvlay-lg`'s `discovery` feature pulls `tisvlay-ssdp`).
7. **After the first publish, set up crates.io trusted publishing** for the release-plz job, so later releases need no long-lived token.
8. **Read the first release PR's CHANGELOGs.** release-plz picks a crate's commits by package path, so a crate that was renamed or moved (`tisvlay-protocol` was `tisvlay-wire`) should read as an initial release, not a history cut off at the rename.

### Public API policy

The published crates release in lockstep on the shared workspace version, and at 0.x a breaking change is a minor bump that release-plz makes for us. The rules below keep the breaking changes rare and deliberate.

- **Private by default.** A public item stays public only if a caller outside the crate needs it. Making an item public later is a minor change; making it private later is a breaking one. `tisvlay-lg` keeps its SSAP client, protocol types and endpoint constants private for this reason: firmware moves the protocol, and those moves must not reach its semver.
- **`#[non_exhaustive]`** goes on:
  - error enums and their code enums (core's `Error`, `ConfigError`, `PairingErrorCode` and `DeviceErrorCode`; ssdp's `Error`);
  - `PowerState`, which grows as TVs report new states;
  - options structs (ssdp's `SearchOptions`), which callers build from `default()` and then set fields on;
  - records vendors build through constructors rather than struct literals (`TvState::new()`, `Credentials::new()`), so a new field doesn't break them;
  - the vendor unit structs (`LgBackend`, `LgConnector`, `CecBackend`), constructed through `new()`/`Default` so they can gain state later.

  It stays off enums whose every arm matters to a caller (`AutomateWhen`, `SessionAction`, `EventAction`, `PairingState`, `PairingCommand`, `Transport`, lg's `WebOsScheme`): there, the compiler demanding a new arm is worth more than additive evolution. It also stays off the structs callers build with literals: the records vendors construct (`InputInfo`, `DiscoveredTv`, `PairingField`) and the vendor configs users fill in (`LgVendorConfig`, `CecVendorConfig`, built as `CecVendorConfig { device, ..Default::default() }`). There a new field is a lockstep minor bump, and the attribute would forbid that construction outright. A workspace match on a non-exhaustive enum from another crate needs a fallback arm, and the arm says what an unknown variant means.
- **Public dependencies** are the crates whose types appear in a published API, so their major-version bumps are breaking for us too. Each crate's README lists its own under "Public dependencies" and names the items that expose them. Today those are:
  - core: tokio, uuid, bitflags, serde, toml, serde_json and async-trait;
  - lg and cec: serde, plus core itself;
  - ssdp: futures-core;
  - the protocol: zenoh, core, serde, serde_json and uuid;
  - the SDK: core, the protocol, uuid, serde, serde_json and futures-core.

  zenoh-ext stays out of every signature, but the SDK depends on its `unstable` feature, so it's pinned with a tilde (`~1.10`): that feature switches on zenoh's unstable and internal APIs, which may break within 1.x, so a caret range could pull a breaking minor into a user's build. A pre-1.0 or young dependency gets a newtype instead: `tisvlay-cec`'s `PhysicalAddress` wraps linux-cec's, which keeps linux-cec out of the API. A test-only feature (`tisvlay-cec`'s `test-link`) is outside the crate's semver guarantees and may re-export a dependency's types. Until cargo's `public` dependency marker (RFC 3516) stabilizes, the README list is the record.

## Review triggers

Patterns to revisit when their gating condition fires:

- **winit's SCTK 0.19 path** — re-audit the cargo-deny skip entries it forces on every Slint or winit bump; an upgrade that moves winit to SCTK 0.20 drops them.
- **`rustls`/`ring` getrandom** — re-audit when `rustls` upgrades its default crypto backend or when `ring` updates getrandom.
- **`vergen-gix` reintroduction** — only if `vergen-gix` ever drops the dashmap/imara-diff pins itself, or if `git` becomes unavailable on a build target we care about.
- **Cargo cross-workspace-cache (2026 goal)** — tracked in [concerns/test-ci-cargo-llvm-cov-show-env.md](concerns/test-ci-cargo-llvm-cov-show-env.md). When the nightly preview lands, evaluate whether its cache-key design generalizes to "instrumented vs not"; that would obsolete the show-env workaround.
- **`LLVM_PROFILE_FILE` defaulting in `.cargo/config.toml`** — tracked in [concerns/profraw-stray-files-outside-target.md](concerns/profraw-stray-files-outside-target.md). The workspace defaults this env var so stray instrumented runs (lingering binaries in `target/debug/deps/` from past coverage builds) write under `target/profraw/` instead of leaking `default_<hash>_<pid>.profraw` to per-crate cwds. Cargo-team posture is "user-side responsibility"; revisit if [rust-lang/cargo#13040](https://github.com/rust-lang/cargo/issues/13040) or [#13136](https://github.com/rust-lang/cargo/issues/13136) progress.
- **Switching to nightly Rust** — tracked in [ideas/ci-nightly-build-knobs.md](ideas/ci-nightly-build-knobs.md). Worth re-opening if CI cycle time becomes a real bottleneck for PR turnaround.
- **cargo-shear vs `[lints.cargo]`**: when the toolchain pin reaches the release that stabilizes cargo's own lints, try `[workspace.lints.cargo] unused_dependencies` and `missing_lints_inheritance`. If they cover what shear finds, retire `just shear`.
- **clippy's generic `allow-in-tests`** (rust-clippy#17520): once it ships, replace the per-lint `allow-*-in-tests` keys in `.config/clippy.toml`, and delete the integration-test roots' `#![expect(clippy::unwrap_used, clippy::panic)]`.
- **The yearly pedantic probe**: once a year, run clippy once with `-W clippy::pedantic` (and the nursery lints worth a look) and consider new hand-picked lints for `[workspace.lints]`. No group is ever enabled (§ Lint policy).
- **tombi**: when nixpkgs reaches the pinned version, drop the flake override. Bump the pin and CI's `tombi@<version>` together.
- **zenoh 2.x, and every zenoh-ext bump**: zenoh is a public dependency of `tisvlay-protocol`, so a zenoh major is a breaking release of the protocol and the SDK. zenoh-ext is pinned `~1.10` because the SDK uses its `unstable` API: move the pin deliberately, checking the advanced-subscriber API the SDK's mirrors use.

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
