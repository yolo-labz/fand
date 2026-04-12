#!/usr/bin/env bash
# verify-reproducible-build.sh — T109a / C6 / SC-019 reproducibility check.
#
# Confirms that the fand release binary produced on two different
# machines (developer workstation + GitHub Actions runner) hashes to
# the same SHA-256. A mismatch means SOURCE_DATE_EPOCH,
# toolchain version, or a system dependency has drifted and the
# build is no longer reproducible.
#
# This script is machine-agnostic: it runs `cargo build --release`
# with `SOURCE_DATE_EPOCH` set to the commit timestamp (the same
# recipe used by .github/workflows/ci.yml) and prints the resulting
# SHA-256. It does NOT compare against a remote hash — the caller
# is responsible for running it on both the local machine and the
# CI runner (or downloading a CI artifact) and diff'ing the outputs.
#
# Usage:
#   ./scripts/verify-reproducible-build.sh              # build + hash
#   ./scripts/verify-reproducible-build.sh --expected-sha <SHA>  # assert match
#
# Expected fingerprint (locally produced on 2026-04-12, rustc 1.94.0,
# commit c30706971227925ebd8bbaf6df64dfd1c2d03ff3):
#
#   8d774876adc862b82067db1d38f9fcbdb10a6179cc2b4aab6f4365feba1a53c7
#
# The expected SHA will change on every commit (because build.rs
# embeds FAND_GIT_REV into the binary). The test that matters is
# whether two runs AT THE SAME COMMIT produce the same SHA.

set -euo pipefail

EXPECTED=""
if [ "${1:-}" = "--expected-sha" ]; then
    EXPECTED="${2:-}"
    shift 2
fi

# 1. Pin SOURCE_DATE_EPOCH to the HEAD commit timestamp — matches CI.
epoch=$(git log -1 --format=%ct HEAD)
export SOURCE_DATE_EPOCH="$epoch"
echo "verify-reproducible-build: SOURCE_DATE_EPOCH=${epoch}"

# 2. Show the toolchain so a diverging rustc is obvious.
rustc --version
cargo --version

# 3. Clean any previous release build so stale artifacts do not mask
#    a non-reproducible outcome.
cargo clean --release 2>/dev/null || true

# 4. Build with --locked so Cargo.lock is enforced.
echo "verify-reproducible-build: cargo build --release --locked"
cargo build --release --locked

# 5. Compute the SHA-256 of the resulting binary.
if command -v sha256sum >/dev/null 2>&1; then
    sha=$(sha256sum target/release/fand | awk '{print $1}')
else
    sha=$(shasum -a 256 target/release/fand | awk '{print $1}')
fi

size=$(wc -c < target/release/fand | tr -d ' ')
echo "verify-reproducible-build: sha256 = ${sha}"
echo "verify-reproducible-build: size   = ${size} bytes"

if [ -n "${EXPECTED}" ]; then
    if [ "${sha}" = "${EXPECTED}" ]; then
        echo "verify-reproducible-build: PASS — SHA matches expected"
        exit 0
    fi
    echo "verify-reproducible-build: FAIL — SHA ${sha} != expected ${EXPECTED}" >&2
    exit 1
fi

echo "verify-reproducible-build: complete (no --expected-sha supplied — nothing to compare against)"
