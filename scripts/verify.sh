#!/bin/sh
# Full verification loop for offload. Run before every commit and PR.
# Usage: scripts/verify.sh [--fast] [--coverage]
#   --fast:     skip the release build check
#   --coverage: run tests under cargo-llvm-cov and enforce >=80% line coverage
set -e

cd "$(dirname "$0")/.."

FAST=0
COVERAGE=0
for arg in "$@"; do
    case "$arg" in
        --fast) FAST=1 ;;
        --coverage) COVERAGE=1 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

fail() {
    echo "✗ $1"
    exit 1
}

echo "── cargo check (types)"
cargo check --all-targets >/dev/null || fail "type check failed"

echo "── cargo clippy (lint, deny warnings)"
cargo clippy --all-targets -- -D warnings >/dev/null || fail "lint failed"

if [ "$COVERAGE" -eq 1 ]; then
    if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
        fail "cargo-llvm-cov not installed (install with: cargo install cargo-llvm-cov)"
    fi
    echo "── cargo llvm-cov (tests + >=80% line coverage)"
    cargo llvm-cov --all-targets --workspace \
        --fail-under-lines 80 \
        --ignore-filename-regex 'src/(main\.rs|tui/(mod|app)\.rs|eject\.rs|detect\.rs)' \
        >/dev/null || fail "tests/coverage failed"
else
    echo "── cargo test"
    cargo test --all-targets >/dev/null || fail "tests failed"
fi

echo "── cargo build"
cargo build --all-targets >/dev/null || fail "build failed"

if [ "$FAST" -ne 1 ]; then
    echo "── cargo build --release"
    cargo build --release >/dev/null || fail "release build failed"
fi

echo "✓ all checks passed"
