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

# Format all Rust code
[group('check')]
fmt:
    cargo fmt

# Build the workspace (test profile, all targets, all features)
[group('check')]
build:
    cargo build --profile test --workspace --all-targets --all-features

# Run clippy with warnings denied
[group('check')]
clippy:
    cargo clippy --profile test --workspace --all-targets --all-features -- -D warnings

# Run the test suite with cargo-nextest (parallel, process-per-test isolation)
[group('check')]
test:
    cargo nextest run --workspace --all-features
    cargo test --workspace --all-features --doc

# Minimal feature subset for CI's coverage + clippy path (`test-ci`).
# Chosen as the smallest set that still exercises every `#[cfg(feature
# = "X")]` block in `crates/*/src/` — covered either directly by `mock`
# or transitively (`discovery`, `dbus`, `logging` are reached through
# `tisvlayd/mock`'s default + workspace consumers). Keeping it minimal
# holds the coverage-instrumented build under the runner's ~14 GB
# free-disk ceiling; `--all-features` would compile far more under
# instrumentation. prek locally still runs `--all-features` clippy.
#
# When to extend:
# - New `#[cfg(feature = "X")]` block in `src/` where X isn't already
#   `mock` / `discovery` / `dbus` / `logging` → add the feature here
#   so CI lints + tests it (or the gate becomes silent on PRs).
# - New workspace crate with a mock-style test-only feature → add
#   `<crate>/mock` here.
ci_features := "tisvlay-backends/mock,tisvlayd/mock"

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
    cargo clippy --workspace --all-targets \
        --features {{ ci_features }} -- -D warnings
    cargo nextest run --workspace --features {{ ci_features }}
    cargo test --workspace --features {{ ci_features }} --doc
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
    cargo deny check --config packaging/licensing/deny.toml licenses bans

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
# Two translatable crates today: `tisvlay-gui` (Slint UI + Rust) and
# `tisvlay-tray` (Rust only). Each owns its own gettext domain and
# `.pot` template; per-locale `.po` files live under
# `crates/<crate>/lang/<locale>/LC_MESSAGES/<crate>.po`.
#
# `i18n-extract` rebuilds both `.pot` templates from source. The GUI
# pass combines Rust (`gettext("…")` via xgettext) and Slint
# (`@tr("…")` via slint-tr-extractor) into one `.pot`; the tray
# pass is Rust-only. Run this whenever user-facing strings change.
#
# `i18n-update` merges the refreshed `.pot`s into every existing
# per-locale `.po`, marking new entries fuzzy and obsoleting
# removed ones.
#
# `.mo` files are generated at compile time — by `slint_build` for
# the GUI and by `crates/tisvlay-tray/build.rs` for the tray — so
# there's no separate compile recipe. `cargo build` after editing
# a `.po` is enough to refresh runtime catalogs for either crate.
gui_lang_dir := "crates/tisvlay-gui/lang"
gui_pot := gui_lang_dir + "/tisvlay-gui.pot"
tray_lang_dir := "crates/tisvlay-tray/lang"
tray_pot := tray_lang_dir + "/tisvlay-tray.pot"

# Extract translatable strings for both crates into their .pot templates.
[group('check')]
i18n-extract: i18n-extract-gui i18n-extract-tray

# GUI: Rust (`gettext()`) + Slint (`@tr()`) into tisvlay-gui.pot.
i18n-extract-gui:
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

# Tray: Rust-only (`gettext()`) into tisvlay-tray.pot.
i18n-extract-tray:
    @mkdir -p {{ tray_lang_dir }}
    xgettext --language=Rust --keyword=gettext --keyword=ngettext:1,2 \
        --from-code=UTF-8 --package-name=tisvlay-tray \
        --copyright-holder='TisVlay contributors' \
        --output={{ tray_pot }} \
        $(find crates/tisvlay-tray/src -name '*.rs')
    @echo "Wrote {{ tray_pot }} — review and commit."

# Merge the refreshed .pot into every existing per-locale .po file.
[group('check')]
i18n-update: i18n-extract
    @for crate_dir in {{ gui_lang_dir }} {{ tray_lang_dir }}; do \
        domain=$(basename $(dirname $crate_dir)); \
        pot="$crate_dir/$domain.pot"; \
        for po in $crate_dir/*/LC_MESSAGES/$domain.po; do \
            if [ -f "$po" ]; then \
                echo "Merging $pot into $po"; \
                msgmerge --update --backup=none "$po" "$pot"; \
            fi; \
        done; \
    done

# --- Coverage -----------------------------------------------------------

# Baseline line-coverage percentage. Bump on PRs that improve coverage.
cov_baseline_lines := "52"

# Exclude test infrastructure from coverage reports - measure production
# code only. Matches crate names and test-only modules.
cov_exclude := "--ignore-filename-regex '(tisvlay-(test|mock|harness)/|test_server)'"

# Run tests under coverage instrumentation (shared by all cov-* recipes).
# Mirrors `test-ci` — see that recipe's comment for the
# `CARGO_PROFILE_DEV_DEBUG=line-tables-only` rationale, and the
# `ci_features` comment for the feature-subset rationale.
[private]
_cov-collect:
    CARGO_PROFILE_DEV_DEBUG=line-tables-only \
        cargo llvm-cov --no-report nextest --workspace \
            --features {{ ci_features }}
    cargo test --workspace --features {{ ci_features }} --doc

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
# Each peer has a single `<peer>-start` recipe parameterized by the
# top-level `bus` / `gui_mcp` variables below. Edit defaults for your
# usual workflow, or override per-call via env or --set:
#
#   BUS=system just daemon-start          # env-var override
#   just --set bus system daemon-start    # --set override
#   GUI_MCP=true just gui-start           # GUI with Slint MCP on :9315
#
# These are dev-only; no production / CI path goes through them.

# "user" = session/user bus, "system" = system bus (daemon needs sudo).
bus := env("BUS", "user")

# GUI only. "true" → build with `--features slint/mcp` and start the
# Slint MCP server on http://127.0.0.1:9315/mcp. See `.claude/slint.md`.
gui_mcp := env("GUI_MCP", "false")

_daemon_log := "debug,zbus=warn,rustls=warn,tungstenite=warn"
_session_log := "debug,zbus=warn"
_tray_log := "debug,zbus=warn,ksni=warn"
_gui_log := "debug,zbus=warn,slint=warn,winit=warn"

# `--user` flag for binaries that take it. Empty when bus=system, so
# the binary defaults to the system bus.
_user_flag := if bus == "user" { "--user" } else { "" }

# `sudo cargo run` for the system bus would write artifacts into
# root's target dir, so the system path pre-builds as the user and
# then sudo-runs the resulting binary.
#
# Start the daemon.
[group('dev')]
daemon-start:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{ bus }}" = "system" ]; then
        cargo build --bin tisvlayd
        RUST_LOG="{{ _daemon_log }}" \
            sudo -E ./target/debug/tisvlayd \
                --config docs/configs/daemon.toml
    else
        RUST_LOG="{{ _daemon_log }}" \
            cargo run --bin tisvlayd -- --session \
                --config docs/configs/daemon.toml
    fi

# Start the session service.
[group('dev')]
session-start:
    RUST_LOG="{{ _session_log }}" \
        cargo run --bin tisvlay-session -- {{ _user_flag }} \
            --config docs/configs/session.toml

# Start the tray.
[group('dev')]
tray-start:
    RUST_LOG="{{ _tray_log }}" \
        cargo run --bin tisvlay-tray -- {{ _user_flag }} \
            --config docs/configs/tray.toml

# Start the GUI (paired with `daemon-start`).
[group('dev')]
gui-start:
    {{ if gui_mcp == "true" { "SLINT_MCP_PORT=9315" } else { "" } }} \
        RUST_LOG="{{ _gui_log }}" \
        cargo run {{ if gui_mcp == "true" { "--features slint/mcp" } else { "" } }} \
        --bin tisvlay-gui -- {{ _user_flag }}

# Kill all running dev processes
[group('dev')]
stop-all:
    -pkill -f 'target/debug/tisvlayd'
    -pkill -f 'target/debug/tisvlay-session'
    -pkill -f 'target/debug/tisvlay-tray'
    -pkill -f 'target/debug/tisvlay-gui'
    # The SIGTERM above returns immediately, but the daemon takes
    # a beat to release its `org.tisvlay.Daemon` bus name. A fresh
    # `tisvlayd` started by `all: restart` would race against that
    # release. Block until all four binaries are actually gone so
    # the VS Code task chain `[all: stop, all: start]` with
    # `dependsOrder: sequence` does the right thing on its own. 5 s
    # timeout (`|| true`) so a stuck process can't wedge the recipe
    # indefinitely.
    @timeout 5 sh -c 'while pgrep -f "target/debug/(tisvlayd|tisvlay-(session|tray|gui))" > /dev/null; do sleep 0.1; done' || true
    @echo "All stopped"

# Show CLI status (manual smoke test)
[group('dev')]
cli-status:
    cargo run --bin tisvlay -- --user status

# --- Packaging ----------------------------------------------------------

# Min glibc the produced binaries support. Coverage table and bump
# rationale: packaging/README.md.
glibc_baseline := "2.34"

# Cross-distro release build — see packaging/README.md for the full
# pipeline rationale. Output lands in target/x86_64-unknown-linux-gnu
# /release/; zigbuild strips the `.<glibc>` suffix when invoking cargo.
# Crate scoping comes from `workspace.default-members` (the shipping
# binaries) in the root Cargo.toml — no `-p` flags repeated here. The
# `mock` feature ships the daemon's "Mock (testing)" field-debug backend.
[private]
package-build:
    cargo zigbuild --release \
        --target x86_64-unknown-linux-gnu.{{ glibc_baseline }} \
        --features tisvlayd/mock

# Build .deb package
[group('package')]
package-deb: licenses-html package-build
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-gnu -f deb

# Build .rpm package
[group('package')]
package-rpm: licenses-html package-build
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-gnu -f rpm

# Build Arch Linux package
[group('package')]
package-archlinux: licenses-html package-build
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-gnu -f archlinux

# Build all package formats
[group('package')]
package-all: package-deb package-rpm package-archlinux
