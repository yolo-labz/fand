# Security Policy

## Supported Versions

Only the latest tagged release of `fand` receives security updates.
Pre-release/development builds from `main` are best-effort.

| Version  | Supported          |
| -------- | ------------------ |
| latest   | :white_check_mark: |
| < latest | :x:                |

## Reporting a Vulnerability

**Please do NOT open public GitHub issues for security vulnerabilities.**

Use one of these private channels:

1. **GitHub Security Advisories (preferred)** — open a private advisory at
   https://github.com/yolo-labz/fand/security/advisories/new
2. **Email** — contact the maintainer directly via the email listed on
   https://github.com/phsb5321

### What to include

- Affected version (commit SHA or release tag)
- Reproduction steps or proof-of-concept
- Impact assessment (what data/system is at risk)
- Suggested mitigation (optional)

### Response SLA

- **Acknowledgement:** within 72 hours
- **Triage + initial assessment:** within 7 days
- **Fix or mitigation:** target 30 days for high/critical, 90 days for medium/low

We will credit reporters in the release notes unless anonymity is requested.

## Verifying Releases

Every release is published with cryptographic provenance via Sigstore.
Verify a downloaded release artifact:

```bash
gh attestation verify <artifact> --repo yolo-labz/fand
```

SBOMs (CycloneDX 1.7 + SPDX 2.3) are attached to each GitHub Release for
supply-chain auditing.

## Threat Model

`fand` is a userspace fan-control daemon for Linux that reads hwmon
temperature sensors and writes PWM values to control system fans. It
runs with elevated privileges (typically as a systemd service with
direct hwmon access) and trusts:

- The kernel's hwmon interface for sensor readings
- Local configuration files (TOML) under `/etc/fand` or `$XDG_CONFIG_HOME`
- The systemd unit it ships with for privilege boundaries

Out-of-scope for this project:
- Network-level threats (no network surface)
- Multi-user host hardening (single-admin assumption)
- Hardware fault tolerance beyond fail-safe defaults
- Resistance to malicious local root (root can already control fans directly)
