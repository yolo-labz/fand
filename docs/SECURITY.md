# fand — Security Policy

This file is the authoritative source for how fand handles security
reports, CVEs, dependency rollbacks, and the supply-chain attestation
surface. It implements **FR-078** (exact-version pinning),
**FR-079** (CVE rollback protocol), and **FR-101** (6-month review
cadence) from the feature 005 specification.

## Reporting a vulnerability

Email **pedrohbalbino@users.noreply.github.com** with the subject line
`[fand security] <short description>`. Please do **not** open a public
GitHub issue for an unpatched vulnerability.

We prefer reports that include:

- affected version (the output of `fand --version` is ideal)
- a minimal reproducer (command + host model + macOS build)
- threat classification (arbitrary-fan-command, privilege escalation,
  denial-of-thermal-service, etc.)
- your timeline preferences (CVD window, disclosure date, credit text)

We will acknowledge receipt **within 48 hours** and aim to ship a
patched release **within 7 calendar days** for anything rated HIGH or
CRITICAL in the threat model
(`specs/005-smc-write-roundtrip/threat-model.md`). Lower-severity
issues follow the normal release cadence.

## Response ownership

| Role                          | Owner                                   |
|-------------------------------|-----------------------------------------|
| Vulnerability triage          | Pedro H S Balbino                       |
| Rollback decision authority   | Pedro H S Balbino                       |
| CVE reservation + publication | Pedro H S Balbino                       |
| Downstream notification       | Pedro H S Balbino                       |

There is exactly one maintainer today. When that changes, this table
changes with it — not the other way around.

## CVE monitoring SLA

| Severity | Acknowledge | Patched release | Public advisory |
|----------|-------------|-----------------|-----------------|
| CRITICAL | 48 h        | 7 days          | within 24 h of patch |
| HIGH     | 48 h        | 14 days         | within 72 h of patch |
| MEDIUM   | 7 days      | 30 days         | with next tagged release |
| LOW      | 14 days     | next release    | with next tagged release |

The SLA clock starts when we receive the report. If you do not hear
from us within the acknowledge window, please escalate by emailing
again with `[fand security][ESCALATED]` in the subject.

## Dependency rollback protocol (FR-078 + FR-079)

fand pins **every direct dependency to an exact version** in
`Cargo.toml` — no caret ranges, no tilde ranges, no wildcards. This is
enforced pre-merge by the `cargo deny` job in
`.github/workflows/ci.yml` and audited by `cargo vet` against the
attestations in `supply-chain/audits.toml`.

When a new CVE lands against one of our direct or transitive
dependencies, the rollback decision tree is:

1. **Is there a patched upstream version available?**
   - **Yes** — bump `Cargo.toml` to the new exact version, run
     `cargo update -p <crate>`, run the full CI matrix (ci.yml +
     miri.yml + the nightly soak), update
     `supply-chain/audits.toml` with a new `[[audits.CRATE]]` entry
     for the new version, and ship a patch release.
   - **No** — go to step 2.

2. **Is the vulnerable crate reachable from the fand hot path?**
   - Run `cargo tree --invert --edges features` to confirm reachability.
   - If it is **unreachable** (test-only, dev-only, or behind a feature
     we don't enable) — document the finding in the release notes and
     wait for an upstream patch.
   - If it is **reachable** — go to step 3.

3. **Is there a prior safe version we can roll back to?**
   - **Yes** — pin the older version in `Cargo.toml`, re-run
     `cargo vet` (which MUST pass because the older version was already
     in our audit set), ship a patch release, and file an upstream
     issue asking for a patched version of the current major.
   - **No** — go to step 4.

4. **Vendor a fork.** Fork the crate at the last safe commit, apply
   a minimal patch for the CVE, publish it as
   `fand-<crate>-patched = { version = "=X.Y.Z-fand.1" }` under the
   `pedrohbalbino` GitHub account, pin that version in `Cargo.toml`,
   and document the fork in `supply-chain/audits.toml` with an
   explicit `criteria = ["safe-to-deploy", "emergency-fork"]` entry.
   This is the last resort; the goal is always to return to upstream
   as soon as a patched release is available.

5. **Rollback announcement.** Every rollback — whether a version bump,
   a downgrade, or a fork — must be called out in the release notes
   with:
   - the CVE identifier (if one exists)
   - the severity rating
   - the affected versions of fand
   - the exact dependency version change made
   - the attestation of the `cargo vet` pass over the new state

## Re-evaluation cadence (FR-101)

The full dependency tree is re-audited every **6 months**. The next
scheduled re-audit is **2026-10-01**. The re-audit rewrites
`supply-chain/audits.toml` from scratch:

- Every direct dependency gets a fresh `who` + `version` + `notes`
  entry based on the then-current pinned version.
- Every new indirect dependency that has appeared since the last
  audit gets a new entry.
- Entries for removed dependencies are deleted, not commented out.

The re-audit date is also when we re-evaluate whether any "temporary"
dependencies (e.g., `fs4` per the FR-101 note in `Cargo.toml`) should
be replaced by `libc::flock` directly or by a hand-rolled equivalent.

## Scope boundaries

What is **in scope** for this policy:

- the fand binary itself (`target/release/fand`)
- every direct Rust dependency in `Cargo.toml`
- the nix-darwin module at `nix/module.nix` (once it ships)
- the sandbox profile at `nix/sandbox-profiles/fand-set.sb`
- the signed release artifacts distributed via GitHub Releases
- the cargo-vet attestations in `supply-chain/audits.toml`

What is **out of scope**:

- Apple's IOKit, AppleSMC user client, and firmware behaviour (the
  threat model documents the assumptions we make; flaws in Apple's
  attack surface should be reported to Apple Product Security directly)
- `cargo install`-based installation (we cannot audit the user's
  system cargo, rustup, or PATH)
- custom forks not distributed by the upstream `yolo-labz/fand`
  repository
