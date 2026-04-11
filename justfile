# tisvlay project tasks
#
# Run `just` (or `just --list`) to see all recipes.
# This justfile is the canonical source of truth for project commands —
# prek hooks, VSCode tasks, and CI workflows all dispatch through here.

# Default recipe: list all available recipes.
default:
    @just --list

# --- Quality checks (prek hooks + CI) -----------------------------------

# Format all Rust code
fmt:
    cargo fmt

# Build the workspace (test profile, all targets, all features)
build:
    cargo build --profile test --workspace --all-targets --all-features

# Run clippy with warnings denied
clippy:
    cargo clippy --profile test --workspace --all-targets --all-features -- -D warnings

# Run the test suite
test:
    cargo test --workspace --all-features

# Run all fast checks in order (what prek runs)
check: fmt build clippy test

# --- Coverage -----------------------------------------------------------

# Baseline line-coverage percentage. Bump on PRs that improve coverage.
cov_baseline_lines := "19"

# Text summary of workspace coverage
cov:
    cargo llvm-cov --workspace --all-features --summary-only

# HTML coverage report (opens in browser)
cov-html:
    cargo llvm-cov --workspace --all-features --html --open

# Generate lcov.info for VS Code Coverage Gutters
cov-lcov:
    cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info
    @echo "Wrote lcov.info — point VS Code Coverage Gutters at it."

# Ratchet check — fails if below baseline (used by CI)
cov-ratchet:
    cargo llvm-cov --workspace --all-features \
        --fail-under-lines {{ cov_baseline_lines }} --summary-only

# --- Dev run targets ----------------------------------------------------

_daemon_log := "debug,zbus=warn,rustls=warn,tungstenite=warn"
_session_log := "debug,zbus=warn"
_tray_log := "debug,zbus=warn,ksni=warn"

# Start the daemon on the session bus (dev mode)
daemon-start:
    RUST_LOG="{{ _daemon_log }}" cargo run --bin tisvlayd -- --session --config docs/configs/daemon.toml

# Start the daemon on the system bus (requires sudo)
daemon-start-system:
    cargo build --bin tisvlayd
    RUST_LOG="{{ _daemon_log }}" sudo -E ./target/debug/tisvlayd --config docs/configs/daemon.toml

# Start the session service on the user session bus
session-start:
    RUST_LOG="{{ _session_log }}" cargo run --bin tisvlay-session -- --user --config docs/configs/session.toml

# Start the session service on the system bus
session-start-system:
    RUST_LOG="{{ _session_log }}" cargo run --bin tisvlay-session -- --config docs/configs/session.toml

# Start the tray on the user session bus
tray-start:
    RUST_LOG="{{ _tray_log }}" cargo run --bin tisvlay-tray -- --user

# Start the tray on the system bus
tray-start-system:
    RUST_LOG="{{ _tray_log }}" cargo run --bin tisvlay-tray

# Kill all running dev processes
stop-all:
    -pkill -f 'target/debug/tisvlayd'
    -pkill -f 'target/debug/tisvlay-session'
    -pkill -f 'target/debug/tisvlay-tray'
    @echo "All stopped"

# Show CLI status (manual smoke test)
cli-status:
    cargo run --bin tisvlay -- --user status

# --- Packaging ----------------------------------------------------------

# Build musl static binaries for packaging. Writes to
# target/x86_64-unknown-linux-musl/release/ and never touches
# the native dev build cache at target/release/.
package-build-musl:
    cargo build --release --workspace --target x86_64-unknown-linux-musl

# Build .deb package
package-deb: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f deb

# Build .rpm package
package-rpm: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f rpm

# Build Arch Linux package
package-archlinux: package-build-musl
    cargo nfpm package -p tisvlayd --no-build --target x86_64-unknown-linux-musl -f archlinux

# Build all package formats
package-all: package-deb package-rpm package-archlinux
