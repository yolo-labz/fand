#!/usr/bin/env bash
# run-mutants.sh — local cargo-mutants driver for T096 / FR-085.
#
# This script is a thin wrapper around `cargo mutants` that:
#
#   1. Ensures cargo-mutants is installed (installs from crates.io if
#      not present — one-time setup).
#   2. Runs mutants on the four hot modules only, in full-tree mode.
#   3. Captures the caught score into `target/mutants/score.txt` so
#      the commit message / release notes can cite it.
#   4. Asserts the caught score is at least 85% (FR-085).
#
# The full run takes anywhere from 30 minutes to several hours on a
# developer workstation — it is NOT intended for pre-merge CI. Use
# `.github/workflows/nightly.yml` for the CI job.
#
# Usage:
#   ./scripts/run-mutants.sh                 # full-tree on hot modules
#   ./scripts/run-mutants.sh --in-diff main  # only mutants against main
#
# The wrapper honors `.cargo/mutants.toml` for shared config.

set -euo pipefail

TARGET_DIR="${CARGO_TARGET_DIR:-target}/mutants"
MIN_CAUGHT_PERCENT=85

if ! command -v cargo-mutants >/dev/null 2>&1; then
    echo "run-mutants: cargo-mutants not installed — installing now..." >&2
    cargo install --locked cargo-mutants
fi

echo "run-mutants: starting full-tree mutation run on the four hot modules"
echo "run-mutants: target directory = ${TARGET_DIR}"

mkdir -p "${TARGET_DIR}"

# The four hot modules listed in T096 + the additional modules declared
# in .cargo/mutants.toml. cargo-mutants will pick the config up from
# the repo root automatically.
cargo mutants \
    --timeout-multiplier 2.0 \
    --output "${TARGET_DIR}" \
    "$@"

# Parse the score from the summary. cargo-mutants writes an
# `outcomes.json` file with the caught/missed tallies.
if [ -f "${TARGET_DIR}/outcomes.json" ]; then
    caught=$(jq -r '.caught // 0' "${TARGET_DIR}/outcomes.json")
    missed=$(jq -r '.missed // 0' "${TARGET_DIR}/outcomes.json")
    total=$((caught + missed))
    if [ "$total" -eq 0 ]; then
        echo "run-mutants: WARNING — no mutants evaluated" >&2
        exit 0
    fi
    pct=$((caught * 100 / total))
    echo "run-mutants: caught=${caught} missed=${missed} total=${total} score=${pct}%"
    printf '%d\n' "${pct}" > "${TARGET_DIR}/score.txt"

    if [ "$pct" -lt "$MIN_CAUGHT_PERCENT" ]; then
        echo "run-mutants: FAIL — caught score ${pct}% below ${MIN_CAUGHT_PERCENT}% floor (FR-085)" >&2
        exit 1
    fi
    echo "run-mutants: PASS — ${pct}% >= ${MIN_CAUGHT_PERCENT}% (FR-085)"
else
    echo "run-mutants: outcomes.json not emitted — rerun with --json to parse a score" >&2
    exit 1
fi
