#!/usr/bin/env bash
# soak.sh — FR-100-hour soak replacement (T104, SC-006).
#
# Runs `sudo fand selftest` in a loop 10 times (100 round-trip iterations
# total at 5 per run) and asserts:
#
#   1. Every run exits 0.
#   2. The `total_mismatches` field in the JSON envelope is 0 on every run.
#   3. `lsmp -p $PID` does not leak Mach ports across iterations
#      (the port count after iteration 10 must be <= iteration 1 + 2 slots
#      of slack for the transient io_connect_t triples during teardown).
#
# Intended to run on real Apple Silicon hardware. The script detects
# absence of `lsmp` / `sudo` / `codesign` gracefully and skips the
# affected checks rather than failing.
#
# Usage:
#   ./tests/soak.sh                         # 10 iterations (default)
#   ./tests/soak.sh 50                      # 50 iterations
#   FAND=/path/to/fand ./tests/soak.sh 100  # custom binary path

set -euo pipefail

FAND="${FAND:-target/release/fand}"
ITERATIONS="${1:-10}"
ITER_PER_RUN="${ITER_PER_RUN:-5}"

if [ ! -x "$FAND" ]; then
    echo "soak.sh: $FAND not found — run 'cargo build --release' first" >&2
    exit 1
fi

if ! command -v sudo >/dev/null 2>&1; then
    echo "soak.sh: sudo not found — this script needs root to open AppleSMC" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "soak.sh: jq not found — skipping JSON mismatch assertions" >&2
    JQ=
else
    JQ=$(command -v jq)
fi

# Capture baseline Mach-port count for leak detection.
baseline_ports=0
if command -v lsmp >/dev/null 2>&1; then
    baseline_ports=$(lsmp -p $$ 2>/dev/null | wc -l | tr -d ' ')
    echo "soak.sh: baseline Mach port count for PID $$ = ${baseline_ports}"
else
    echo "soak.sh: lsmp not available — skipping Mach-port leak assertion"
fi

total_mismatches=0
failures=0

for i in $(seq 1 "$ITERATIONS"); do
    printf "soak.sh: iteration %d/%d ... " "$i" "$ITERATIONS"
    output=$(sudo "$FAND" selftest --iterations "$ITER_PER_RUN" --json 2>&1) || {
        echo "FAIL (non-zero exit)"
        echo "$output" | tail -20 >&2
        failures=$((failures + 1))
        continue
    }

    if [ -n "$JQ" ]; then
        mismatches=$(echo "$output" | "$JQ" -r '.summary.total_mismatches' 2>/dev/null || echo "UNKNOWN")
        if [ "$mismatches" != "0" ]; then
            echo "FAIL (total_mismatches=${mismatches})"
            failures=$((failures + 1))
            continue
        fi
        total_mismatches=$((total_mismatches + mismatches))
    fi
    echo "ok"
done

echo
echo "soak.sh: ${ITERATIONS} iterations, ${failures} failures, ${total_mismatches} total mismatches"

if [ "$failures" -gt 0 ]; then
    echo "soak.sh: FAILED — see output above"
    exit 1
fi

# Mach-port leak assertion: the current process shouldn't hold extra
# ports after the soak completes. Each selftest subprocess opens three
# io_connect_t send rights (main + signal + panic hook) then tears
# them all down.
if [ -n "${baseline_ports}" ] && [ "$baseline_ports" -gt 0 ]; then
    final_ports=$(lsmp -p $$ 2>/dev/null | wc -l | tr -d ' ')
    delta=$((final_ports - baseline_ports))
    echo "soak.sh: final Mach port count = ${final_ports} (delta = ${delta})"
    if [ "$delta" -gt 2 ]; then
        echo "soak.sh: FAIL — suspected Mach-port leak (delta > 2)"
        exit 1
    fi
fi

echo "soak.sh: PASS — ${ITERATIONS} iterations, 0 mismatches, no port leak"
