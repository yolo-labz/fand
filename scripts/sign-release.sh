#!/usr/bin/env bash
# sign-release.sh — ad-hoc codesign the fand release binary (FR-067, C5).
#
# Runs after `cargo build --release`. Ad-hoc signs the resulting binary
# with the hardened runtime flag so macOS Gatekeeper recognizes it as
# a well-formed Mach-O bundle. This is a SHOULD not a MUST per FR-067:
# if `codesign` is not available (e.g., non-Darwin CI), the script
# skips silently and exits 0.
#
# Usage:
#   ./scripts/sign-release.sh [path/to/fand]
#
# If no path is given, defaults to target/release/fand.
#
# Why ad-hoc signing (no Apple Developer ID)?
# - fand is FOSS and explicitly opts out of the paid Apple notary pipeline.
# - Ad-hoc signing is sufficient for hardened-runtime enforcement on the
#   installing machine — it is NOT enough for Gatekeeper quarantine
#   transit. Users who install via `cargo install` or a nix-darwin module
#   never hit the quarantine bit, so this is fine for our distribution
#   model.
# - See docs/ARCHITECTURE.md §Packaging for the full rationale.

set -euo pipefail

BINARY="${1:-target/release/fand}"

if [ ! -f "$BINARY" ]; then
    echo "sign-release.sh: $BINARY does not exist — did you run cargo build --release?" >&2
    exit 1
fi

if ! command -v codesign >/dev/null 2>&1; then
    # Non-Darwin CI or a stripped-down macOS install without the
    # Command Line Tools. FR-067 says SHOULD, not MUST — skip silently.
    echo "sign-release.sh: codesign not found — skipping (FR-067 SHOULD)" >&2
    exit 0
fi

# Verify this is actually a Mach-O binary before we try to sign it.
if ! file "$BINARY" | grep -q 'Mach-O'; then
    echo "sign-release.sh: $BINARY is not a Mach-O binary — refusing to sign" >&2
    exit 1
fi

# Ad-hoc sign with hardened runtime.
#
# -s -  : use the ad-hoc signing identity (no certificate required)
# -o runtime : enable the hardened runtime flag
# -f    : replace any existing signature
# -v    : verbose output for logs
codesign -s - -o runtime -f -v "$BINARY"

# Sanity check: the binary should now carry a valid signature.
codesign --verify --verbose=2 "$BINARY"

# Optional: print the signature flags for release notes.
codesign --display --verbose=2 "$BINARY" 2>&1 | grep -E '^(Identifier|Format|CodeDirectory|Signature size|Sealed Resources|Info\.plist|TeamIdentifier|flags)' || true

echo "sign-release.sh: $BINARY ad-hoc signed with hardened runtime"
