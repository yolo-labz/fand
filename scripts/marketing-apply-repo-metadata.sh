#!/usr/bin/env bash
# scripts/marketing-apply-repo-metadata.sh
#
# Idempotently sets the marketing surface on the GitHub repo:
#   - Description (≤120 chars, capability-only framing)
#   - Topics (3–10, GitHub topic-discovery surface)
#
# Run from an authenticated `gh` shell (gh auth login first). Safe to re-run;
# `gh api` PATCH/PUT are idempotent on identical payloads. Source of truth for
# the marketing copy is README.md `## Capability` block + `## How fand compares`.
#
# Provenance: shipped with PR #20 (feat(marketing): hero + capability +
# comparison + asciinema + OG card). Class-leader pattern: yolo-labz/wa#172.

set -euo pipefail

REPO="${REPO:-yolo-labz/fand}"

DESCRIPTION="Apple Silicon fan control daemon. Temperature-driven curves. SLSA L2 signed. nix-darwin module."

# 8 topics, GitHub topic-discovery surface. Order does not matter; GitHub stores
# them lowercase + sorted on retrieval.
TOPICS_JSON='{"names":["apple-silicon","fan-control","rust","macos","nix-darwin","slsa","daemon","launchd"]}'

echo "→ patching description on ${REPO}"
gh api -X PATCH "repos/${REPO}" -f description="${DESCRIPTION}" --jq '.description'

echo "→ putting topics on ${REPO}"
printf '%s' "${TOPICS_JSON}" | gh api -X PUT "repos/${REPO}/topics" --input - --jq '.names | join(", ")'

echo "✓ done"
