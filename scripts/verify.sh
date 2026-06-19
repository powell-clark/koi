#!/usr/bin/env bash
# koi pre-push quality gate — the local CI triad.
# Runs cargo fmt --check, clippy -D warnings, and the test suite, failing fast.
# Mirrors GitHub CI so red code never reaches metered runners. Usable standalone
# (./scripts/verify.sh) or via githooks/pre-push. No machine-specific paths — part
# of the publishable scripts/ surface.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bold() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

bold "cargo fmt --check"
cargo fmt --all -- --check

bold "cargo clippy -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

bold "cargo test"
cargo test --workspace

printf '\n\033[1;32mverify.sh: all checks passed.\033[0m\n'
