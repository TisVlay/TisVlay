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

# Run tests under coverage instrumentation (CI): produces JUnit XML via
# the 'ci' nextest profile, coverage data, then generates lcov.info and
# checks the ratchet - all from one test run. Doc tests run separately
# without coverage (stable rustc doesn't support doctest coverage).
[group('check')]
test-ci:
    NEXTEST_PROFILE=ci cargo llvm-cov --no-report nextest --workspace --all-features
    cargo test --workspace --all-features --doc
    cargo llvm-cov report {{ cov_exclude }} --lcov --output-path lcov.info
    cargo llvm-cov report {{ cov_exclude }} --fail-under-lines {{ cov_baseline_lines }} --summary-only

# Run all fast checks in order (what prek runs)
[group('check')]
check: fmt build clippy test

# Review pending snapshot changes (insta)
[group('check')]
test-snapshot-review:
    cargo insta review

# Accept all pending snapshot changes (insta)
[group('check')]
test-snapshot-accept:
    cargo insta accept

# --- Coverage -----------------------------------------------------------

# Baseline line-coverage percentage. Bump on PRs that improve coverage.
cov_baseline_lines := "49"

# Exclude test infrastructure from coverage reports - measure production
# code only. Matches crate names and test-only modules.
cov_exclude := "--ignore-filename-regex '(tisvlay-(test|mock|harness)/|test_server)'"

# Run tests under coverage instrumentation (shared by all cov-* recipes).
[private]
_cov-collect:
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

_daemon_log := "debug,zbus=warn,rustls=warn,tungstenite=warn"
_session_log := "debug,zbus=warn"
_tray_log := "debug,zbus=warn,ksni=warn"

# Start the daemon on the session bus (dev mode)
[group('dev')]
daemon-start:
    RUST_LOG="{{ _daemon_log }}" cargo run --bin tisvlayd -- --session --config docs/configs/daemon.toml

# Start the daemon on the system bus (requires sudo)
[group('dev')]
daemon-start-system:
    cargo build --bin tisvlayd
    RUST_LOG="{{ _daemon_log }}" sudo -E ./target/debug/tisvlayd --config docs/configs/daemon.toml

# Start the session service on the user session bus
[group('dev')]
session-start:
    RUST_LOG="{{ _session_log }}" cargo run --bin tisvlay-session -- --user --config docs/configs/session.toml

# Start the session service on the system bus
[group('dev')]
session-start-system:
    RUST_LOG="{{ _session_log }}" cargo run --bin tisvlay-session -- --config docs/configs/session.toml

# Start the tray on the user session bus
[group('dev')]
tray-start:
    RUST_LOG="{{ _tray_log }}" cargo run --bin tisvlay-tray -- --user

# Start the tray on the system bus
[group('dev')]
tray-start-system:
    RUST_LOG="{{ _tray_log }}" cargo run --bin tisvlay-tray

# Kill all running dev processes
[group('dev')]
stop-all:
    -pkill -f 'target/debug/tisvlayd'
    -pkill -f 'target/debug/tisvlay-session'
    -pkill -f 'target/debug/tisvlay-tray'
    @echo "All stopped"

# Show CLI status (manual smoke test)
[group('dev')]
cli-status:
    cargo run --bin tisvlay -- --user status

# --- Packaging ----------------------------------------------------------

# Build musl static binaries for packaging. Writes to
# target/x86_64-unknown-linux-musl/release/ and never touches
# the native dev build cache at target/release/.
[private]
package-build-musl:
    cargo build --release --workspace --target x86_64-unknown-linux-musl

# Build .deb package
[group('package')]
package-deb: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f deb

# Build .rpm package
[group('package')]
package-rpm: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f rpm

# Build Arch Linux package
[group('package')]
package-archlinux: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f archlinux

# Build all package formats
[group('package')]
package-all: package-deb package-rpm package-archlinux
