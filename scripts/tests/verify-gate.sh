#!/usr/bin/env bash
# Behavioural test for scripts/verify.sh — the pre-push quality gate.
# Asserts: red on a tree with a formatting violation, green on a clean tree.
# Restores the perturbed file unconditionally.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
verify="$repo_root/scripts/verify.sh"

if [ ! -x "$verify" ]; then
  echo "RED: scripts/verify.sh missing or not executable — gate does not exist yet"
  exit 1
fi

victim="crates/koi-core/src/lib.rs"
backup="$(mktemp)"
cp "$victim" "$backup"
restore() { cp "$backup" "$victim"; rm -f "$backup"; }
trap restore EXIT

# Introduce a deliberate rustfmt violation (bad spacing). Valid Rust, mis-formatted.
printf '\n   fn   __verify_gate_violation__ ( )  {  }\n' >> "$victim"
if "$verify" >/dev/null 2>&1; then
  echo "FAIL: verify.sh passed on a tree with a formatting violation"
  exit 1
fi
echo "ok: verify.sh blocks a formatting-violation tree"

restore
trap - EXIT
if ! "$verify" >/dev/null 2>&1; then
  echo "FAIL: verify.sh failed on a clean tree"
  exit 1
fi
echo "ok: verify.sh passes on a clean tree"

echo "verify-gate harness: PASS"
