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

# Format all Rust (rustfmt) and TOML (tombi; config in tombi.toml)
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
# (config: `_typos.toml`).
[group('check')]
typos:
    typos

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
# Each run clears stale `.profraw` first so `report` aggregates only
# this run. No rebuild cost. If coverage still looks off, escalate
# to `cargo clean` (compile-cache, separate axis).
[group('check')]
test-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_PROFILE_DEV_DEBUG=line-tables-only
    export NEXTEST_PROFILE=ci
    export {{ deny_warnings }}
    export CARGO_TARGET_DIR=target/llvm-cov-target
    eval "$(cargo llvm-cov show-env --sh)"
    cargo llvm-cov clean --profraw-only
    cargo clippy --workspace --all-targets --all-features
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    cargo llvm-cov report {{ cov_exclude }} \
        --fail-under-lines {{ cov_baseline_lines }} --summary-only

# Run all fast checks in order (what prek runs)
[group('check')]
check: deny shear typos licenses-html fmt build clippy doc test

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

# Whole-workspace SBOM (SPDX 2.3) from the committed Cargo.lock, written under
# target/sbom/ (gitignored). A supply-chain artifact, not a gate — the CI job
# uploads it and it is available on demand. cargo-sbom emits one document to
# stdout, so a single redirect is the whole recipe.
sbom_dir := "target/sbom"

[group('check')]
sbom:
    mkdir -p {{ sbom_dir }}
    cargo sbom --output-format spdx_json_2_3 > {{ sbom_dir }}/tisvlay.spdx.json
    @echo "wrote {{ sbom_dir }}/tisvlay.spdx.json"

# --- i18n / translation catalogs ---------------------------------------
#
# One translatable crate today: `tisvlay-gui` (Slint UI + Rust). It owns
# its gettext domain and `.pot` template; per-locale `.po` files live
# under `crates/tisvlay-gui/lang/<locale>/LC_MESSAGES/tisvlay-gui.po`.
# The tray's user-facing strings (menu items, notification copy) are
# not gettext-wired yet — tracked in docs/ideas.md.
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
# gates the partials' freshness instead.

# Generated include partials.
generated_docs := "docs/book/src/generated"

# Build the docs site into docs/book/book/ (gitignored). Installs the
# mdbook-mermaid JS assets first (gitignored, regenerated each build).
[group('docs')]
docs-build:
    mdbook-mermaid install docs/book
    mdbook build docs/book

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
cov_baseline_lines := "52"

# Exclude test infrastructure from coverage reports - measure production
# code only. Matches crate names and test-only modules.
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
        cargo run --release --bin tisvlay-daemon -- \
            --socket "{{ socket }}" --site "{{ site }}" \
            --ssdp-port "{{ ssdp_port }}" {{ _ephemeral_flag }} "${dpms[@]}"

# Start the session service (user socket owner; lock/idle dispatch).
[group('dev')]
session-start:
    RUST_LOG="{{ _session_log }}" \
        cargo run --release --bin tisvlay-session -- \
            --system-socket "{{ socket }}" --user-socket "{{ user_socket }}" \
            --site "{{ site }}" {{ _ephemeral_flag }}

# Start the greeter idle client (needs a wayland compositor).
[group('dev')]
greeter-start:
    RUST_LOG="{{ _greeter_log }}" \
        cargo run --release --bin tisvlay-greeter -- \
            --socket "{{ socket }}" --site "{{ site }}"

# Start the tray (user profiles appear when session-start is up).
[group('dev')]
tray-start:
    RUST_LOG="{{ _tray_log }}" \
        cargo run --release --bin tisvlay-tray -- \
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
    # SLINT_EMIT_DEBUG_INFO (needed for the MCP element tree) is set
    # globally in .cargo/config.toml, so only the port + feature here.
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
        cargo run "${feature_arg[@]}" --release --bin tisvlay-gui -- \
            --socket "{{ socket }}" --user-socket "{{ user_socket }}" \
            --site "{{ site }}"

# Kill all running dev processes
[group('dev')]
stop-all:
    -pkill -f 'tisvlay-daemon'
    -pkill -f 'tisvlay-session'
    -pkill -f 'tisvlay-greeter'
    -pkill -f 'tisvlay-tray'
    -pkill -f 'tisvlay-gui'
    # SIGTERM returns immediately; wait until the processes are gone so
    # a follow-up start can't race the exit. Stale socket files are
    # fine — zenoh's `.lock` protocol takes over an unclean listener
    # path on the next bind. 5 s timeout (`|| true`) so a stuck
    # process can't wedge the recipe.
    @timeout 5 sh -c 'while pgrep -f "target/debug/tisvlay-(daemon|session|greeter|tray|gui)" > /dev/null; do sleep 0.1; done' || true
    @echo "All stopped"

# Show CLI status (manual smoke test)
[group('dev')]
cli-status:
    cargo run --release --bin tisvlay -- --socket "{{ socket }}" --site "{{ site }}" status

# Run any CLI verb against the dev sockets, e.g. `just cli list`.
[group('dev')]
cli *args:
    cargo run --release --bin tisvlay -- \
        --socket "{{ socket }}" --user-socket "{{ user_socket }}" \
        --site "{{ site }}" {{ args }}

# --- Packaging ----------------------------------------------------------

# Min glibc the produced binaries support. Coverage table and bump
# rationale: packaging/README.md.
glibc_baseline := "2.34"

# Cross-distro release build — see packaging/README.md for the full
# pipeline rationale. Output lands in target/x86_64-unknown-linux-gnu
# /release/; zigbuild strips the `.<glibc>` suffix when invoking cargo.
# Crate scoping comes from `workspace.default-members` (the shipping
# binaries) in the root Cargo.toml — no `-p` flags repeated here.
[private]
package-build:
    cargo zigbuild --release \
        --target x86_64-unknown-linux-gnu.{{ glibc_baseline }}

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
    cargo nfpm package -p tisvlay-daemon --no-build --target x86_64-unknown-linux-gnu -f deb

# Build .rpm package
[group('package')]
package-rpm: licenses-html completions package-build
    cargo nfpm package -p tisvlay-daemon --no-build --target x86_64-unknown-linux-gnu -f rpm

# Build Arch Linux package
[group('package')]
package-archlinux: licenses-html completions package-build
    cargo nfpm package -p tisvlay-daemon --no-build --target x86_64-unknown-linux-gnu -f archlinux

# Build all package formats
[group('package')]
package-all: package-deb package-rpm package-archlinux
