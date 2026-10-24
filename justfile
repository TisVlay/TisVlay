# tisvlay project tasks
#
# Run `just` (or `just --list`) to see all recipes, grouped by purpose.
# This justfile is the canonical source of truth for project commands -
# prek hooks, VSCode tasks, and CI workflows all dispatch through here.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Default recipe: list all available recipes, grouped.
default:
    @just --list --unsorted

# --- Quality checks (prek hooks + CI) -----------------------------------

# Shared build + clippy scope, kept in one place so the two recipes can't
# drift: test profile, whole workspace, all targets + all features.
check_flags := "--profile test --workspace --all-targets --all-features"

# Format all Rust (rustfmt) and TOML (tombi; config in .config/tombi.toml)
[group('check')]
fmt:
    cargo fmt
    tombi format --quiet

# Build the workspace (test profile, all targets, all features)
[group('check')]
build:
    cargo build {{ check_flags }}

# The gates deny warnings through cargo's `build.warnings` rather than
# `-- -D warnings`: it covers clippy, test and rustdoc builds alike, fails
# replayed (cached) warnings too, and toggling it doesn't invalidate the
# build cache. It is set per gate recipe, never in `.cargo/config.toml` —
# the Nix package build reads that file with nixpkgs' rustc, and a newer
# compiler's new warning must not break a NixOS rebuild. Local `cargo
# build`/`cargo run` stay warn-only.
deny_warnings := "CARGO_BUILD_WARNINGS=deny"

# Run clippy with warnings denied
[group('check')]
clippy:
    {{ deny_warnings }} cargo clippy {{ check_flags }}

# Build the API docs with warnings denied (broken or private intra-doc
# links). `--no-deps`: only the workspace's own docs are checked.
# `--profile test` reuses the dependency metadata `just clippy` built.
[group('check')]
doc:
    {{ deny_warnings }} cargo doc --profile test --workspace --no-deps --all-features

# Find unused or misplaced dependencies. cargo-shear parses the sources
# (no build), so it runs in seconds; false positives go under a crate's
# `[package.metadata.cargo-shear] ignored`, with a comment saying why.
[group('check')]
shear:
    cargo shear --deny-warnings

# Spell-check code, docs and identifiers, enforcing American English
# (config: `.config/typos.toml`). typos doesn't search `.config/` yet, so
# the path is explicit; drop `--config` once crate-ci/typos#1445 lands.
[group('check')]
typos:
    typos --config .config/typos.toml

# Lint the GitHub workflows (config: `.config/actionlint.yaml`): syntax,
# expressions, runner labels and, through shellcheck, the `run:` scripts.
[group('check')]
actionlint:
    actionlint -config-file .config/actionlint.yaml

# Audit the workflows and the Dependabot config for security problems:
# unpinned actions, persisted credentials, broad permissions, injection.
# With a GitHub token in GH_TOKEN (CI sets one), zizmor adds its online
# audits, such as a pinned SHA that doesn't match its version comment;
# without one it runs offline and says so.
[group('check')]
zizmor:
    zizmor .github/

# Run the test suite with cargo-nextest (parallel, process-per-test isolation)
[group('check')]
test:
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc

# Run the full check pipeline (clippy + tests + doc tests + coverage
# report) under a single coverage-instrumented build. `cargo llvm-cov
# show-env --sh` emits `export VAR=…` lines that route every later
# cargo command through cargo-llvm-cov's RUSTC_WRAPPER, which adds
# `-C instrument-coverage` and points LLVM_PROFILE_FILE into the
# target dir; `eval`-ing them in this bash recipe means clippy, nextest
# and the doc tests all share one instrumented build. Design + the
# residual clippy-vs-build dual-compile finding live in
# `docs/build.md` "Build pipeline".
#
# CARGO_TARGET_DIR is pinned to `target/llvm-cov-target/` before the
# `eval`. show-env doesn't choose a directory itself, and
# cargo doesn't rebuild when only RUSTC_WRAPPER changes, so sharing
# `target/` would reuse an uninstrumented `just build`: no `.profraw`,
# and a failed report.
#
# `CARGO_PROFILE_DEV_DEBUG=line-tables-only` shrinks the instrumented
# `target/llvm-cov-target/` to fit the runner's free-disk ceiling.
# `llvm-cov` only needs line tables to map regions to source.
#
# Doc tests run under the same env, but stable rustc doesn't emit
# coverage data for doctest harnesses — they execute but don't
# contribute to the report.
#
# Each run first clears the workspace crates' instrumented artifacts
# and stale `.profraw`. `report` reads every test binary in the target
# dir, and one built from older source counts its files' regions again,
# uncovered, pulling the total below what this run covered. The clean
# costs a rebuild of the workspace crates, which CI does anyway
# (rust-cache doesn't keep them); dependencies stay cached.
# `_cov-collect` gets the same clean from `cargo llvm-cov`'s default.
[group('check')]
test-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_PROFILE_DEV_DEBUG=line-tables-only
    export NEXTEST_PROFILE=ci
    export {{ deny_warnings }}
    export CARGO_TARGET_DIR=target/llvm-cov-target
    eval "$(cargo llvm-cov show-env --sh)"
    cargo llvm-cov clean --workspace
    cargo clippy --workspace --all-targets --all-features
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    cargo llvm-cov report {{ cov_exclude }} \
        --fail-under-lines {{ cov_baseline_lines }} --summary-only

# Run all fast checks in order (what prek runs)
[group('check')]
check: deny shear typos actionlint zizmor licenses-html fmt build clippy doc test

# --- Slow / networked checks (CI jobs) ---------------------------------
#
# These run in CI, not in `just check` or prek: each takes minutes cold
# or needs the network. The recipes exist to reproduce a CI failure
# locally. Background: `docs/build.md` "Checks that run in CI".

# RustSec advisories + yanked crates (network). Weekly in CI and on
# Cargo.lock changes; triaged advisories live in deny.toml's ignore-list.
[group('ci')]
audit:
    cargo deny --config packaging/licensing/deny.toml check advisories

# Build every crate with each feature on its own (and with none), since
# every other gate builds with --all-features. `--no-dev-deps` makes
# cargo-hack strip dev-dependencies from the manifests for the run and
# restore them after, so rust-analyzer may flag errors mid-run.
[group('ci')]
hack:
    cargo hack check --each-feature --no-dev-deps --workspace --target-dir target/hack

# Check the workspace on its MSRV floor: the flake's `msrv-rust`
# toolchain (read from Cargo.toml's `rust-version`) first on PATH, inside
# the dev shell for the system libraries. No `--ignore-rust-version`: the
# toolchain IS the floor, so a dependency that outgrows it fails here.
[group('ci')]
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    tc=$(nix build --no-link --print-out-paths .#msrv-rust)
    PATH="$tc/bin:$PATH" cargo check --workspace --all-targets --all-features --target-dir target/msrv

# --- License attribution / policy --------------------------------------
#
# Pipeline rationale + maintenance reference: `docs/licensing.md`.

# Enforce the SPDX allow-list (`licenses`), code provenance (`sources`)
# and one version per crate (`bans`, `multiple-versions = "deny"`). A
# dependency update that adds a duplicate fails here; the bump routine
# is in `docs/build.md` "Dep-graph posture". `-D unnecessary-skip -D
# unmatched-skip` make a stale skip-list entry an error too, so the
# list can't outlive the duplicates it excuses.
[group('check')]
deny:
    cargo deny --config packaging/licensing/deny.toml check \
        -D unnecessary-skip -D unmatched-skip licenses bans sources

# Regenerate THIRD-PARTY-LICENSES.html for packaging (gitignored;
# consumed by the package-* recipes). Doubles as the cargo-about
# license gate: `--all-features --fail` surfaces any crate whose
# license expression isn't covered by `about.toml` (e.g. a Slint bump
# pulling a new renderer into the all-features graph). Wired into
# `check` and a gated prek hook so the gate runs on dep / about.toml
# changes — not just at distro-package build time.
[group('check')]
licenses-html:
    cargo about generate \
        --config packaging/licensing/about.toml \
        --workspace --all-features --locked --fail \
        packaging/licensing/about.html.hbs \
        -o packaging/licensing/THIRD-PARTY-LICENSES.html

# --- i18n / translation catalogs ---------------------------------------
#
# One translatable crate today: `tisvlay-gui` (Slint UI + Rust). It owns
# its gettext domain and `.pot` template; per-locale `.po` files live
# under `crates/tisvlay-gui/lang/<locale>/LC_MESSAGES/tisvlay-gui.po`.
# The tray's user-facing strings (menu items, notification copy) are
# not gettext-wired yet — tracked in docs/ideas/switchover-followups.md.
#
# `i18n-extract` rebuilds the `.pot` template from source. The pass
# combines Rust (`gettext("…")` via xgettext) and Slint (`@tr("…")`
# via slint-tr-extractor) into one `.pot`. Run this whenever
# user-facing strings change.
#
# `i18n-update` merges the refreshed `.pot` into every existing
# per-locale `.po`, marking new entries fuzzy and obsoleting
# removed ones.
#
# `.mo` files are generated at compile time by `slint_build`, so
# there's no separate compile recipe. `cargo build` after editing
# a `.po` is enough to refresh runtime catalogs.
gui_lang_dir := "crates/tisvlay-gui/lang"
gui_pot := gui_lang_dir + "/tisvlay-gui.pot"

# Extract translatable strings into the .pot template.
[group('check')]
i18n-extract:
    @mkdir -p {{ gui_lang_dir }}
    # Rust pass: xgettext on every gettext()/ngettext() call site.
    xgettext --language=Rust --keyword=gettext --keyword=ngettext:1,2 \
        --from-code=UTF-8 --package-name=tisvlay-gui \
        --copyright-holder='TisVlay contributors' \
        --output={{ gui_pot }} \
        $(find crates/tisvlay-gui-app/src -name '*.rs')
    # Slint pass: append @tr() strings to the same .pot via --join-existing.
    slint-tr-extractor --join-existing --default-domain=tisvlay-gui \
        --package-name=tisvlay-gui \
        -o {{ gui_pot }} \
        $(find crates/tisvlay-gui/ui -name '*.slint')
    @echo "Wrote {{ gui_pot }} — review and commit."

# Merge the refreshed .pot into every existing per-locale .po file.
[group('check')]
i18n-update: i18n-extract
    @for po in {{ gui_lang_dir }}/*/LC_MESSAGES/tisvlay-gui.po; do \
        if [ -f "$po" ]; then \
            echo "Merging {{ gui_pot }} into $po"; \
            msgmerge --update --backup=none "$po" "{{ gui_pot }}"; \
        fi; \
    done

# --- Documentation site (mdBook → Read the Docs) ------------------------
#
# User-facing docs live in docs/book/ (book.toml + src/), separate from
# the developer-facing docs/*.md. The partials under src/generated/ (the
# CLI reference, the stock profile defaults, the release version) are
# rendered from the code by the tisvlay-docgen crate and committed; Read
# the Docs builds a plain `mdbook build`, so it never runs cargo — CI
# gates the partials' freshness and the built book's local links/fragments.

# Generated include partials.
generated_docs := "docs/book/src/generated"

# Build the docs site into docs/book/book/ (gitignored). Installs the
# mdbook-mermaid JS assets first (gitignored, regenerated each build).
[group('docs')]
docs-build:
    mdbook-mermaid install docs/book
    mdbook build docs/book

# Check local links and heading fragments in the rendered HTML. External
# links are skipped so this gate is deterministic and needs no network.
[group('docs')]
docs-links: docs-build
    lychee --offline --include-fragments=anchor-only \
        --root-dir "$PWD/docs/book/book" docs/book/book/

# Serve the docs site with live reload at http://localhost:3000.
[group('docs')]
docs-serve:
    mdbook-mermaid install docs/book
    mdbook serve docs/book --open

# Regenerate every generated partial from the code it documents.
[group('docs')]
docs-gen:
    cargo run -q -p tisvlay-docgen -- {{ generated_docs }}
    @echo "Wrote {{ generated_docs }}/ — review and commit."

# Fail if a committed partial is stale or a new one is uncommitted (CI
# drift gate).
[group('docs')]
docs-gen-check: docs-gen
    git diff --exit-code -- {{ generated_docs }}
    test -z "$(git ls-files --others --exclude-standard -- {{ generated_docs }})"

# --- Coverage -----------------------------------------------------------

# Baseline line-coverage percentage. Bump on PRs that improve coverage.
cov_baseline_lines := "84"

# Exclude separate test infrastructure crates/files from coverage reports.
# Inline #[cfg(test)] modules share source files with production and remain
# in the llvm-cov denominator; see docs/testing.md.
cov_exclude := "--ignore-filename-regex '(tisvlay-(test|mock)/|test_server)'"

# Where `_cov-collect` records the test run's exit status, so the report
# recipes can still render coverage after a failure and then exit with it.
cov_status := "target/llvm-cov-target/cov-test-status"

# Fail with the recorded test status, if the run had failures.
cov_verdict := 'status=$(cat ' + cov_status + ' 2>/dev/null || echo 1); if [ "$status" != 0 ]; then echo "tests failed (exit $status) - coverage reflects a failing run" >&2; exit "$status"; fi'

# Run tests under coverage instrumentation (shared by all cov-* recipes).
# Mirrors `test-ci` — see that recipe's comment for the
# `CARGO_PROFILE_DEV_DEBUG=line-tables-only` rationale. Every test runs
# (`--no-fail-fast`) and a failure is recorded rather than aborting, so
# the report recipes still render what ran; they exit with the recorded
# status afterwards. The gate (`test-ci`) stays fail-fast.
[private]
_cov-collect:
    #!/usr/bin/env bash
    set -uo pipefail
    export CARGO_PROFILE_DEV_DEBUG=line-tables-only
    status=0
    cargo llvm-cov --no-report nextest --workspace --all-features --no-fail-fast || status=$?
    cargo test --workspace --all-features --doc || { rc=$?; [ "$status" = 0 ] && status=$rc; }
    mkdir -p "$(dirname {{ cov_status }})"
    echo "$status" > {{ cov_status }}

# Text summary of workspace coverage
[group('coverage')]
cov: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --summary-only
    @{{ cov_verdict }}

# HTML coverage report (opens in browser)
[group('coverage')]
cov-html: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --html --open
    @{{ cov_verdict }}

# Generate lcov.info for VS Code Coverage Gutters
[group('coverage')]
cov-lcov: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    @echo "Wrote lcov.info - point VS Code Coverage Gutters at it."
    @{{ cov_verdict }}

# Ratchet check - fails if below `lines` percent (default: cov_baseline_lines).
# A failing test run fails the ratchet before any comparison: partial
# coverage must neither pass nor move the baseline.
[group('coverage')]
cov-ratchet lines=cov_baseline_lines: _cov-collect
    @{{ cov_verdict }}
    cargo llvm-cov report {{ cov_exclude }} --fail-under-lines {{ lines }} --summary-only

# --- Dev run targets ----------------------------------------------------
#
# Every peer takes the same socket/site parameters, defaulted below so
# a bare `just daemon-start` + `just session-start` + `just tray-start`
# find each other with no flags. Override per-call via env or --set:
#
#   SITE=home just daemon-start                # env-var override
#   just --set site home daemon-start          # --set override
#   DPMS_CONNECTOR=card1-HDMI-A-1 just daemon-start
#   EPHEMERAL=true just daemon-start           # no state file
#   GUI_MCP=true just gui-start                # Slint MCP on :9315
#
# These are dev-only; no production / CI path goes through them.

runtime_dir := env("XDG_RUNTIME_DIR", "/tmp")

# System-daemon socket every client connects to.
socket := env("TISVLAY_SOCKET", runtime_dir / "tisvlay/zenoh.sock")

# Per-user session-service socket (user profiles live behind it).
user_socket := env("TISVLAY_USER_SOCKET", runtime_dir / "tisvlay/session.sock")

# Site chunk used in all key expressions.
site := env("SITE", "main")

# "true" → daemon/session run without persistence.
ephemeral := env("EPHEMERAL", "false")

# Extra DRM connector to watch for DPMS transitions, dispatched to
# every automated TV (e.g. card1-HDMI-A-1) — the override for mocks
# and failed EDID matches. EDID-matched connectors are watched
# automatically, scoped per TV. Empty = automatic watchers only.
dpms_connector := env("DPMS_CONNECTOR", "")

# Pinned SSDP source port for LG discovery — must match the firewall
# rule (docs/firewall.md); 0 = ephemeral.
ssdp_port := env("SSDP_PORT", "1901")

# Cargo profile for the dev-run recipes. `test` reuses what `just build`,
# clippy and `just test` already compiled, keeps debug assertions on, and
# keeps the Slint element ids the MCP server needs (tisvlay-gui's build.rs
# emits them for every non-release profile). RELEASE=true runs the
# shipped profile instead, to check release-only behavior.
_dev_profile := if env("RELEASE", "false") == "true" { "release" } else { "test" }

# GUI only. "true" → build with `--features slint/mcp` and start the
# Slint MCP server on http://127.0.0.1:9315/mcp. See `.claude/slint.md`.
gui_mcp := env("GUI_MCP", "true")

# GUI only. "true" → build with `--features slint/live-preview` so the
# running binary reloads ui/*.slint from disk on every save (no cargo
# rebuild), preserving properties/models/callbacks. Combine with
# GUI_MCP=true to inspect the live-reloaded UI over MCP.
gui_live := env("GUI_LIVE", "true")

# zenoh is chatty at debug; keep app crates loud and transports quiet.
_daemon_log := "info,tisvlay_daemon=debug,tisvlay_world=debug,tisvlay_cec=debug,zbus=warn,rustls=warn,tungstenite=warn"
_session_log := "info,tisvlay_session=debug,zbus=warn"
_greeter_log := "info,tisvlay_greeter=debug"
_tray_log := "info,tisvlay_tray=debug,zbus=warn,ksni=warn"
_gui_log := "info,tisvlay_gui=debug,slint=warn,winit=warn"

_ephemeral_flag := if ephemeral == "true" { "--ephemeral" } else { "" }

# Start the daemon (system socket owner).
[group('dev')]
daemon-start:
    #!/usr/bin/env bash
    set -euo pipefail
    dpms=()
    if [ -n "{{ dpms_connector }}" ]; then
        dpms=(--dpms-connector "{{ dpms_connector }}")
    fi
    RUST_LOG="{{ _daemon_log }}" \
        cargo run --profile {{ _dev_profile }} --bin tisvlay -- service daemon \
            --socket "{{ socket }}" --site "{{ site }}" \
            --ssdp-port "{{ ssdp_port }}" {{ _ephemeral_flag }} "${dpms[@]}"

# Start the session service (user socket owner; lock/idle dispatch).
[group('dev')]
session-start:
    RUST_LOG="{{ _session_log }}" \
        cargo run --profile {{ _dev_profile }} --bin tisvlay -- service session \
            --system-socket "{{ socket }}" --user-socket "{{ user_socket }}" \
            --site "{{ site }}" {{ _ephemeral_flag }}

# Start the greeter idle client (needs a wayland compositor).
[group('dev')]
greeter-start:
    RUST_LOG="{{ _greeter_log }}" \
        cargo run --profile {{ _dev_profile }} --bin tisvlay -- service greeter \
            --socket "{{ socket }}" --site "{{ site }}"

# Start the tray (user profiles appear when session-start is up).
[group('dev')]
tray-start:
    RUST_LOG="{{ _tray_log }}" \
        cargo run --profile {{ _dev_profile }} --bin tisvlay -- service tray \
            --socket "{{ socket }}" --user-socket "{{ user_socket }}" \
            --site "{{ site }}"

# Start the GUI (paired with `daemon-start`). GUI_MCP / GUI_LIVE toggle
# the Slint MCP server and in-app .slint hot reload; both are slint
# dependency features passed on the command line, so a plain build never
# drags them in. Rebuild once with a feature on, then (GUI_LIVE) edit
# ui/*.slint and the running window reloads without touching cargo.
[group('dev')]
gui-start:
    #!/usr/bin/env bash
    set -euo pipefail
    env_prefix=(RUST_LOG="{{ _gui_log }}")
    features=()
    # The MCP element tree needs Slint's debug info, which tisvlay-gui's
    # build.rs emits for the test profile (not RELEASE=true), so only the
    # port + feature here.
    if [ "{{ gui_mcp }}" = "true" ]; then
        env_prefix+=(SLINT_MCP_PORT=9315)
        features+=("slint/mcp")
    fi
    if [ "{{ gui_live }}" = "true" ]; then
        env_prefix+=(SLINT_LIVE_PREVIEW=1)
        features+=("slint/live-preview")
    fi
    feature_arg=()
    if [ "${#features[@]}" -gt 0 ]; then
        feature_arg=(--features "$(IFS=,; echo "${features[*]}")")
    fi
    # `env` (not a bare prefix): assignment-looking words from an array
    # expansion aren't treated as env assignments by bash — env applies
    # them explicitly.
    env "${env_prefix[@]}" \
        cargo run "${feature_arg[@]}" --profile {{ _dev_profile }} --bin tisvlay-gui -- \
            --socket "{{ socket }}" --user-socket "{{ user_socket }}" \
            --site "{{ site }}"

# Kill all running dev processes
[group('dev')]
stop-all:
    # Matched on the dev build's path, so packaged services keep running.
    -pkill -f 'target/(debug|release)/tisvlay service (daemon|session|greeter|tray)'
    -pkill -f 'target/(debug|release)/tisvlay-gui'
    # SIGTERM returns immediately; wait until the processes are gone so
    # a follow-up start can't race the exit. Stale socket files are
    # fine — zenoh's `.lock` protocol takes over an unclean listener
    # path on the next bind. 5 s timeout (`|| true`) so a stuck
    # process can't wedge the recipe.
    @timeout 5 sh -c 'while pgrep -f "target/(debug|release)/(tisvlay service (daemon|session|greeter|tray)|tisvlay-gui)" > /dev/null; do sleep 0.1; done' || true
    @echo "All stopped"

# Show CLI status (manual smoke test)
[group('dev')]
cli-status:
    cargo run --profile {{ _dev_profile }} --bin tisvlay -- --socket "{{ socket }}" --site "{{ site }}" status

# Run any CLI verb against the dev sockets, e.g. `just cli list`.
[group('dev')]
cli *args:
    cargo run --profile {{ _dev_profile }} --bin tisvlay -- \
        --socket "{{ socket }}" --user-socket "{{ user_socket }}" \
        --site "{{ site }}" {{ args }}

# --- Packaging ----------------------------------------------------------

# Min glibc the produced binaries support. Coverage table and bump
# rationale: packaging/README.md.
glibc_baseline := "2.34"

# Architecture the distro packages target: the host's by default, so each
# CI runner builds its own natively. ARCH=aarch64 on an x86_64 host
# cross-builds, which needs the aarch64-unknown-linux-gnu Rust target.
package_arch := env("ARCH", arch())
_package_triple := package_arch + "-unknown-linux-gnu"

# Cross-distro release build — see packaging/README.md for the full
# pipeline rationale. Builds with the `dist` profile (Cargo.toml: size
# over build time, since only CI builds the packages). Output lands in
# target/<arch>-unknown-linux-gnu/dist/; zigbuild strips the `.<glibc>`
# suffix when invoking cargo.
# Crate scoping comes from `workspace.default-members` (the shipping
# binaries) in the root Cargo.toml — no `-p` flags repeated here.
# cargo-auditable embeds each binary's dependency list (a `.dep-v0`
# section), which scanners such as `cargo audit bin`, Trivy and grype read,
# and which `just sbom-check` compares with the SBOMs.
[private]
package-build:
    cargo auditable zigbuild --profile dist \
        --target {{ _package_triple }}.{{ glibc_baseline }}
    ln -sfn {{ _package_triple }}/dist target/nfpm-dist

# Generate the CLI's shell-completion bootstraps (one per shell) into
# packaging/completions/ for nfpm to ship. Built host-native on purpose:
# the zigbuild release artifact targets a foreign glibc/interpreter and
# won't run on the NixOS build host, while the registration script is a
# shell-name-only template, so the debug build's output is identical to
# release. (Nix regenerates these itself via installShellCompletion.)
[group('package')]
completions:
    cargo build -p tisvlay-cli
    mkdir -p packaging/completions
    COMPLETE=bash target/debug/tisvlay > packaging/completions/tisvlay.bash
    COMPLETE=zsh  target/debug/tisvlay > packaging/completions/tisvlay.zsh
    COMPLETE=fish target/debug/tisvlay > packaging/completions/tisvlay.fish

# Build .deb package
[group('package')]
package-deb: licenses-html completions package-build
    cargo nfpm package -p tisvlay-cli --no-build --profile dist --target {{ _package_triple }} -f deb

# Build .rpm package
[group('package')]
package-rpm: licenses-html completions package-build
    cargo nfpm package -p tisvlay-cli --no-build --profile dist --target {{ _package_triple }} -f rpm

# Build Arch Linux package
[group('package')]
package-archlinux: licenses-html completions package-build
    cargo nfpm package -p tisvlay-cli --no-build --profile dist --target {{ _package_triple }} -f archlinux

# Build all package formats
[group('package')]
package-all: package-deb package-rpm package-archlinux

sbom_dir := "target/sbom"

# One SBOM per shipped binary for the host architecture's release build
# (CycloneDX 1.5 JSON, with licenses and checksums), written to target/sbom/
# as <binary>-<version>-<arch>.cdx.json; the release attaches them. Each is
# limited to its binary's target and default features, as the release build
# is. cargo-cyclonedx works from cargo metadata, not the build, so
# `just sbom-check` compares them with the built binaries. It writes one file
# per workspace binary next to each crate's Cargo.toml, so the recipe keeps
# the shipped two and deletes the rest. SOURCE_DATE_EPOCH (the commit time)
# makes reruns identical, but it also makes cargo-cyclonedx leave out the
# serial number, which actions/attest requires of a CycloneDX SBOM. So each
# gets a name-based UUID (v5) for its commit, binary and architecture: the
# same build reruns to the same serial, and any other build gets a new one.
[group('package')]
sbom:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf {{ sbom_dir }} && mkdir -p {{ sbom_dir }}
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) cargo cyclonedx --quiet \
        --describe binaries --target {{ _package_triple }} \
        --format json --spec-version 1.5
    commit=$(git rev-parse HEAD)
    for shipped in tisvlay-cli/tisvlay tisvlay-gui-app/tisvlay-gui; do
        crate=${shipped%/*} bin=${shipped#*/}
        generated=crates/$crate/${bin}_bin.cdx.json
        version=$(jq -r .metadata.component.version "$generated")
        serial=$(uuidgen --sha1 --namespace @url \
            --name "https://github.com/TisVlay/TisVlay/commit/$commit#$bin-{{ package_arch }}")
        jq --arg serial "urn:uuid:$serial" '.serialNumber = $serial' "$generated" \
            > "{{ sbom_dir }}/$bin-$version-{{ package_arch }}.cdx.json"
    done
    rm -f crates/*/*_bin.cdx.json
    ls {{ sbom_dir }}

# Fail if an SBOM lists different crates than its release binary embeds
# (cargo-auditable). Run after `just package-all`. The root crate is the one
# expected difference: the SBOM describes it as the binary itself, in
# metadata.component, so it's left out of the comparison.
[group('package')]
sbom-check: sbom
    #!/usr/bin/env bash
    set -euo pipefail
    status=0
    for sbom in {{ sbom_dir }}/*.cdx.json; do
        bin=$(jq -r .metadata.component.name "$sbom")
        binary=target/nfpm-dist/$bin
        [ -f "$binary" ] || { echo "$binary is missing: run \`just package-all\` first" >&2; exit 1; }
        listed=$(jq -r '.components[] | "\(.name)@\(.version)"' "$sbom" | sort -u)
        embedded=$(rust-audit-info "$binary" | jq -r '.packages[] | select(.root != true) | "\(.name)@\(.version)"' | sort -u)
        if diff -u --label "$sbom" --label "$binary (embedded)" <(echo "$listed") <(echo "$embedded"); then
            echo "$bin: $(wc -l <<< "$listed") crates, SBOM and binary agree"
        else
            status=1
        fi
    done
    exit "$status"

# Verify a release's assets in <dir> the way a user would (the book's Security &
# Supply Chain page), for the tag ref that built them (refs/tags/vX.Y.Z):
#   - the checksums, when checksums-sha256.txt is there;
#   - provenance for every package and for the binaries unpacked from them
#     (what an installed system has), and each binary's SBOM attestation,
#     both looked up on GitHub and checked from the release's Sigstore
#     bundles alone;
#   - that each SBOM asset is the SBOM that was signed.
# release.yml runs it before it creates the draft, and `just verify-release`
# runs it on the draft. The signer path is also in build-packages.yml's
# header and the book; keep the three in step.
[group('package')]
verify-assets dir ref:
    #!/usr/bin/env bash
    set -euo pipefail
    shopt -s failglob
    signer=TisVlay/TisVlay/.github/workflows/build-packages.yml
    identity=(--repo TisVlay/TisVlay --signer-workflow "$signer" --source-ref "{{ ref }}" --deny-self-hosted-runners)
    sbom_type=https://cyclonedx.org/bom
    cd "{{ dir }}"
    if [ -f checksums-sha256.txt ]; then
        sha256sum --quiet -c checksums-sha256.txt
        echo "checksums-sha256.txt: OK"
    fi
    version=$(jq -r .metadata.component.version tisvlay-gui-*-x86_64.cdx.json)
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    # An architecture's three packages must carry identical binaries: unpack
    # them all, and keep one copy per architecture to verify.
    for pkg in tisvlay_*.deb tisvlay-*.rpm tisvlay-*.pkg.tar.zst; do
        case $pkg in
            *_amd64.deb | *.x86_64.rpm | *-x86_64.pkg.tar.zst) arch=x86_64 ;;
            *_arm64.deb | *.aarch64.rpm | *-aarch64.pkg.tar.zst) arch=aarch64 ;;
            *) echo "$pkg: unknown architecture" >&2; exit 1 ;;
        esac
        root=$work/unpacked/$pkg
        mkdir -p "$root" "$work/$arch"
        # rpm members are absolute; bsdtar strips the leading `/` and says so.
        case $pkg in
            *.deb) bsdtar -xOf "$pkg" 'data.tar*' | bsdtar -xf - -C "$root" ;;
            *) if ! err=$(bsdtar -xf "$pkg" -C "$root" 2>&1); then echo "$err" >&2; exit 1; fi ;;
        esac
        for bin in tisvlay tisvlay-gui; do
            if [ ! -f "$work/$arch/$bin" ]; then
                cp "$root/usr/bin/$bin" "$work/$arch/$bin"
            elif ! cmp -s "$root/usr/bin/$bin" "$work/$arch/$bin"; then
                echo "$pkg: its $bin differs from the other $arch packages'" >&2
                exit 1
            fi
        done
    done
    for arch in x86_64 aarch64; do
        deb_arch=$([ "$arch" = x86_64 ] && echo amd64 || echo arm64)
        packages=(tisvlay_*_"$deb_arch".deb tisvlay-*."$arch".rpm tisvlay-*-"$arch".pkg.tar.zst)
        provenance=tisvlay-$version-$arch.provenance.sigstore.json
        for subject in "${packages[@]}" "$work/$arch/tisvlay" "$work/$arch/tisvlay-gui"; do
            gh attestation verify "$subject" "${identity[@]}" > /dev/null
            gh attestation verify "$subject" "${identity[@]}" --bundle "$provenance" > /dev/null
            echo "${subject#"$work"/}: provenance OK"
        done
        for bin in tisvlay tisvlay-gui; do
            sbom=$bin-$version-$arch.cdx.json
            bundle=$bin-$version-$arch.cdx.sigstore.json
            for subject in "$work/$arch/$bin" "${packages[@]}"; do
                gh attestation verify "$subject" "${identity[@]}" --predicate-type "$sbom_type" > /dev/null
                gh attestation verify "$subject" "${identity[@]}" --predicate-type "$sbom_type" --bundle "$bundle" > /dev/null
            done
            signed=$(gh attestation verify "$work/$arch/$bin" "${identity[@]}" --predicate-type "$sbom_type" \
                --bundle "$bundle" --format json --jq '.[0].verificationResult.statement.predicate')
            if ! diff -q <(jq -S . <<< "$signed") <(jq -S . "$sbom") > /dev/null; then
                echo "$sbom differs from the SBOM signed in $bundle" >&2
                exit 1
            fi
            echo "$arch/$bin: SBOM OK, for it and its packages; $sbom is the signed one"
        done
    done

# Download a release's assets (a draft too, with your gh login) into
# target/verify/<tag>/ and run `just verify-assets` on them, plus the cosign
# check the book gives for verifying without a GitHub account. Once the
# release is published, it also checks GitHub's release attestation, which
# immutable releases only create on publishing.
[group('package')]
verify-release tag:
    #!/usr/bin/env bash
    set -euo pipefail
    repo=TisVlay/TisVlay
    dir=target/verify/{{ tag }}
    rm -rf "$dir" && mkdir -p "$dir"
    gh release download "{{ tag }}" --repo "$repo" --dir "$dir"
    just verify-assets "$dir" "refs/tags/{{ tag }}"
    version=$(jq -r .metadata.component.version "$dir"/tisvlay-gui-*-x86_64.cdx.json)
    cosign verify-blob-attestation \
        --bundle "$dir/tisvlay-$version-x86_64.provenance.sigstore.json" \
        --certificate-identity "https://github.com/$repo/.github/workflows/build-packages.yml@refs/tags/{{ tag }}" \
        --certificate-oidc-issuer https://token.actions.githubusercontent.com \
        --type https://slsa.dev/provenance/v1 \
        "$dir/tisvlay_$version-1_amd64.deb"
    if [ "$(gh release view "{{ tag }}" --repo "$repo" --json isDraft --jq .isDraft)" = true ]; then
        echo "{{ tag }} is a draft: rerun once it's published, to check GitHub's release attestation too."
    else
        gh release verify "{{ tag }}" --repo "$repo"
        for asset in "$dir"/*; do
            gh release verify-asset "{{ tag }}" "$asset" --repo "$repo" > /dev/null
        done
        echo "release attestation: OK for every asset"
    fi

# Package the crates.io crates and build each from its packaged source,
# the way crates.io will (docs/build.md "First-publish checklist"). One
# invocation, so cargo resolves their path dependencies on each other
# through a local overlay: a per-crate `cargo package -p tisvlay-protocol`
# fails until tisvlay-core is on crates.io. The list shows what ships.
# Cargo refuses a dirty tree; commit first or pass `--allow-dirty`.
[group('package')]
package-check *args:
    cargo package --list -p tisvlay-core -p tisvlay-protocol -p tisvlay {{ args }}
    cargo package -p tisvlay-core -p tisvlay-protocol -p tisvlay {{ args }}
