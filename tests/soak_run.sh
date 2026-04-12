#!/usr/bin/env bash
# soak_run.sh — 10-minute stability test (T050, SC-001, FR-101).
#
# Runs `sudo fand run --config <config>` under varying thermal load
# and asserts zero round-trip mismatches in stderr output.
#
# Usage:
#   ./tests/soak_run.sh                              # default 600s
#   ./tests/soak_run.sh --duration 120                # 2 minutes
#   FAND=target/release/fand ./tests/soak_run.sh     # custom binary

set -euo pipefail

FAND="${FAND:-target/release/fand}"
CONFIG="${CONFIG:-tests/fixtures/test-curve.toml}"
DURATION="${1:-600}"
LOG_FILE="/tmp/fand-soak-$(date +%s).log"

if [ "${1:-}" = "--duration" ]; then
    DURATION="${2:-600}"
fi

if [ ! -x "$FAND" ]; then
    echo "soak_run.sh: $FAND not found — run 'cargo build --release' first" >&2
    exit 1
fi

echo "soak_run.sh: fand binary = $FAND"
echo "soak_run.sh: config = $CONFIG"
echo "soak_run.sh: duration = ${DURATION}s"
echo "soak_run.sh: log file = $LOG_FILE"

# Start fand in the background.
echo "soak_run.sh: starting fand run..."
sudo FAND_ALLOW_TMP_CONFIG=1 "$FAND" run --config "$CONFIG" >"$LOG_FILE" 2>&1 &
FAND_PID=$!

# Give fand time to start.
sleep 2

if ! kill -0 "$FAND_PID" 2>/dev/null; then
    echo "soak_run.sh: FAIL — fand exited immediately" >&2
    cat "$LOG_FILE" >&2
    exit 1
fi

# FR-101: generate thermal load.
echo "soak_run.sh: generating thermal load (yes per CPU core)..."
NCPU=$(sysctl -n hw.ncpu 2>/dev/null || echo 4)
STRESS_PIDS=""
for _ in $(seq 1 "$NCPU"); do
    yes > /dev/null 2>&1 &
    STRESS_PIDS="$STRESS_PIDS $!"
done

echo "soak_run.sh: stress PIDs: $STRESS_PIDS"
echo "soak_run.sh: running for ${DURATION}s..."

# Wait for the test duration.
sleep "$DURATION"

# Stop stress.
for pid in $STRESS_PIDS; do
    kill "$pid" 2>/dev/null || true
done
wait 2>/dev/null || true

# Stop fand via SIGTERM.
echo "soak_run.sh: stopping fand (SIGTERM)..."
sudo kill -TERM "$FAND_PID" 2>/dev/null || true
sleep 2

# Check for mismatches in the log.
mismatches=$(grep -ci 'mismatch\|MISMATCH\|readback.*mismatch' "$LOG_FILE" 2>/dev/null || echo 0)
watchdog=$(grep -ci 'watchdog\|WATCHDOG' "$LOG_FILE" 2>/dev/null || echo 0)
errors=$(grep -ci 'ERROR\|FATAL' "$LOG_FILE" 2>/dev/null || echo 0)

echo
echo "soak_run.sh: results:"
echo "  mismatches: $mismatches"
echo "  watchdog fires: $watchdog"
echo "  errors: $errors"
echo "  log: $LOG_FILE"

if [ "$mismatches" -gt 0 ]; then
    echo "soak_run.sh: FAIL — ${mismatches} round-trip mismatches detected" >&2
    exit 1
fi

if [ "$watchdog" -gt 0 ]; then
    echo "soak_run.sh: FAIL — watchdog fired ${watchdog} times" >&2
    exit 1
fi

echo "soak_run.sh: PASS — ${DURATION}s under load, 0 mismatches, 0 watchdog fires"
