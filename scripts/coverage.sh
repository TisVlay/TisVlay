#!/usr/bin/env bash
# Generate code coverage report for the workspace.
#
# Usage:
#   ./scripts/coverage.sh           # text summary
#   ./scripts/coverage.sh html      # HTML report (opens in browser)
#   ./scripts/coverage.sh lcov      # lcov.info for IDE coverage gutters
#   ./scripts/coverage.sh ratchet   # fail if below baseline (CI)
#
# Requires the Nix dev shell to provide cargo-llvm-cov, llvm-cov, and llvm-profdata.
# Outside Nix, install with `cargo install cargo-llvm-cov` and ensure
# llvm-tools-preview is available (rustup component or matching system LLVM).

set -euo pipefail

# Baseline ratcheted from the lowest measured workspace coverage.
# Bump on PRs that improve coverage; never lower.
BASELINE_LINES=19

mode="${1:-summary}"

case "$mode" in
  summary)
    cargo llvm-cov --workspace --all-features --summary-only
    ;;
  html)
    cargo llvm-cov --workspace --all-features --html --open
    ;;
  lcov)
    cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info
    echo "Wrote lcov.info — point VS Code Coverage Gutters at it."
    ;;
  ratchet)
    cargo llvm-cov --workspace --all-features \
      --fail-under-lines "$BASELINE_LINES" \
      --summary-only
    ;;
  *)
    echo "Unknown mode: $mode"
    echo "Usage: $0 [summary|html|lcov|ratchet]"
    exit 1
    ;;
esac
