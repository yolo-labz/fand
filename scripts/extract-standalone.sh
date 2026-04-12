#!/usr/bin/env bash
# extract-standalone.sh — extract fand into a clean standalone repo.
#
# Copies source files, excludes development-only artifacts.
# This is a one-time operation for the initial repo creation.
#
# Usage:
#   ./scripts/extract-standalone.sh /path/to/target/repo

set -euo pipefail

TARGET="${1:?Usage: $0 /path/to/target/repo}"

if [ ! -d "$TARGET" ]; then
    echo "extract-standalone: target directory does not exist: $TARGET" >&2
    exit 1
fi

SRC="$(cd "$(dirname "$0")/.." && pwd)"

echo "extract-standalone: source = $SRC"
echo "extract-standalone: target = $TARGET"

# FR-001: files to include.
INCLUDE=(
    Cargo.toml Cargo.lock rust-toolchain.toml build.rs deny.toml
    flake.nix LICENSE .gitignore
)

INCLUDE_DIRS=(
    src tests benches nix scripts docs supply-chain .cargo .github
)

# Copy individual files.
for f in "${INCLUDE[@]}"; do
    if [ -f "$SRC/$f" ]; then
        cp "$SRC/$f" "$TARGET/$f"
        echo "  copied $f"
    fi
done

# Copy directories.
for d in "${INCLUDE_DIRS[@]}"; do
    if [ -d "$SRC/$d" ]; then
        cp -R "$SRC/$d" "$TARGET/$d"
        echo "  copied $d/"
    fi
done

# FR-002: verify exclusions.
for excluded in .specify specs wip CLAUDE.md; do
    if [ -e "$TARGET/$excluded" ]; then
        echo "extract-standalone: WARNING — $excluded exists in target, removing" >&2
        rm -rf "$TARGET/$excluded"
    fi
done

# Generate flake.lock if not present.
if [ ! -f "$TARGET/flake.lock" ]; then
    echo "extract-standalone: generating flake.lock..."
    (cd "$TARGET" && nix flake lock 2>/dev/null || echo "  (nix flake lock skipped — run manually)")
fi

echo
echo "extract-standalone: done. Verify with:"
echo "  cd $TARGET"
echo "  ls -la  # should show: Cargo.toml, flake.nix, src/, nix/, etc."
echo "  ls .specify/ specs/ 2>/dev/null  # should show nothing"
