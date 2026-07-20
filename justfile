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

# Format all Rust code
[group('check')]
fmt:
    cargo fmt

# Build the workspace (test profile, all targets, all features)
[group('check')]
build:
    cargo build {{ check_flags }}

# Run clippy with warnings denied
[group('check')]
clippy:
    cargo clippy {{ check_flags }} -- -D warnings

# Run the test suite with cargo-nextest (parallel, process-per-test isolation)
[group('check')]
test:
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc

# Run the full check pipeline (clippy + tests + doc tests + coverage
# report) under a single coverage-instrumented build. `cargo llvm-cov
# show-env --sh` emits the `export VAR=…` lines for
# RUSTFLAGS=-C instrument-coverage and
# CARGO_TARGET_DIR=target/llvm-cov-target/; `eval`-ing them in this
# bash recipe means subsequent cargo commands all share that target
# dir. Without this, clippy and the coverage build would each
# compile the workspace independently (different RUSTFLAGS →
# different cargo fingerprints, no artifact reuse). Design + the
# residual clippy-vs-build dual-compile finding live in
# `docs/build.md` "Build pipeline".
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
    eval "$(cargo llvm-cov show-env --sh)"
    cargo llvm-cov clean --profraw-only
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    cargo llvm-cov report {{ cov_exclude }} \
        --fail-under-lines {{ cov_baseline_lines }} --summary-only

# Run all fast checks in order (what prek runs)
[group('check')]
check: deny licenses-html fmt build clippy test

# Review pending snapshot changes (insta)
[group('check')]
test-snapshot-review:
    cargo insta review

# Accept all pending snapshot changes (insta)
[group('check')]
test-snapshot-accept:
    cargo insta accept

# --- License attribution / policy --------------------------------------
#
# Pipeline rationale + maintenance reference: `docs/licensing.md`.

# Enforce the SPDX allow-list (`licenses`) and surface multi-version
# dep duplication as warnings (`bans`) — see `deny.toml` for the
# skip-list and `docs/build.md` "Dep-graph posture" for the
# routine. `bans` is `multiple-versions = "warn"`, so it never
# fails the build; output is informational.
[group('check')]
deny:
    cargo deny check --config packaging/licensing/deny.toml licenses bans sources

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
        $(find crates/tisvlay-gui/src -name '*.rs')
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
# the developer-facing docs/*.md. The CLI-reference partial
# (src/guide/cli-generated.md) is generated from the clap definitions
# and committed; Read the Docs builds a plain `mdbook build`, so it never
# runs cargo — CI gates the partial's freshness instead.

# Generated CLI-reference include partial.
cli_doc := "docs/book/src/guide/cli-generated.md"

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

# Regenerate the CLI-reference partial from the clap definitions.
[group('docs')]
docs-cli:
    cargo run -q -p tisvlay-cli --bin gen-cli-docs --features gen-docs > {{ cli_doc }}
    @echo "Wrote {{ cli_doc }} — review and commit."

# Fail if the committed CLI-reference partial is stale (CI drift gate).
[group('docs')]
docs-cli-check: docs-cli
    git diff --exit-code -- {{ cli_doc }}

# --- Coverage -----------------------------------------------------------

# Baseline line-coverage percentage. Bump on PRs that improve coverage.
cov_baseline_lines := "52"

# Exclude test infrastructure from coverage reports - measure production
# code only. Matches crate names and test-only modules.
cov_exclude := "--ignore-filename-regex '(tisvlay-(test|mock)/|test_server)'"

# Run tests under coverage instrumentation (shared by all cov-* recipes).
# Mirrors `test-ci` — see that recipe's comment for the
# `CARGO_PROFILE_DEV_DEBUG=line-tables-only` rationale.
[private]
_cov-collect:
    CARGO_PROFILE_DEV_DEBUG=line-tables-only \
        cargo llvm-cov --no-report nextest --workspace --all-features
    cargo test --workspace --all-features --doc

# Text summary of workspace coverage
[group('coverage')]
cov: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --summary-only

# HTML coverage report (opens in browser)
[group('coverage')]
cov-html: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --html --open

# Generate lcov.info for VS Code Coverage Gutters
[group('coverage')]
cov-lcov: _cov-collect
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    @echo "Wrote lcov.info - point VS Code Coverage Gutters at it."

# Ratchet check - fails if below `lines` percent (default: cov_baseline_lines)
[group('coverage')]
cov-ratchet lines=cov_baseline_lines: _cov-collect
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
gui_mcp := env("GUI_MCP", "false")

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

# Start the GUI (paired with `daemon-start`).
[group('dev')]
gui-start:
    {{ if gui_mcp == "true" { "SLINT_MCP_PORT=9315" } else { "" } }} \
        RUST_LOG="{{ _gui_log }}" \
        cargo run {{ if gui_mcp == "true" { "--features slint/mcp" } else { "" } }} \
        --release --bin tisvlay-gui -- \
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
