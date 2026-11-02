# OSV workflow repair

## Spec

Base: `9b72d0a54e9af2fb57d201603635bfb63c8b13de`. Related update PR: #80.
OSV rejects the obsolete `--skip-git` argument before writing results. The pinned v2.6.0 container reproduces exit 127. Its `--include-git-root` is opt-in (default false), so removal preserves the intended root-git behavior. Pin v2.6.0 including its missing-results guard; preserve vulnerability failure and all existing source scanning.

Inventory: Cargo.lock: 110 packages; no empty-inventory allowance.

## Plan

Change only security workflows and their regression documentation/tests. No application, README, media, visibility, secrets, branch-protection, merge or deployment changes. OpenAI GPT-6 Astra implementation; independent full GLM review and workflow approval belong to the coordinator.

## Tasks and verification

- [x] Reproduce original parse failure and inspect upstream input/CLI contract.
- [x] Run actual v2.6.0 scan against this tree; exit 0. Inventory is described above.
- [x] Run synthetic empty-inventory (exit 0), obsolete-flag (exit 127), and vulnerable requests 2.19.1 (exit 1) checks. Empty-inventory allowance does not silence vulnerabilities.
- [x] `actionlint .github/workflows/osv-scanner.yml` and `git diff --check` pass.
- [ ] Independent review, live CI verification and gated merge disposition.

Durable cross-repo reproducer: `yolo-labz/quality-gates`, branch `010-osv-swarm`, `specs/010-osv-swarm/verify.py`. Run it with this checkout as the argument; it executes the scanner image by digest and saves actual JSON/log evidence. Canonical handoff: `docs/swarm-2026-09-21.md` in that branch. No shared runtime abstraction was added.
