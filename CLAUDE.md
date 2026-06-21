# CLAUDE.md — fand

## Purpose

Apple Silicon fan control daemon. Reads SMC temperature sensors, evaluates per-fan temperature-to-RPM curves with hysteresis and slew limiting, and drives `F0md` (forced-minimum vs auto) on M-series MacBooks. Ships as a nix-darwin LaunchDaemon module.

> On Apple Silicon, the SMC only accepts `F0md=0` (auto) and `F0md=1` (forced minimum); arbitrary RPM targets are read-only. The curve output reduces to a binary decision per tick. See `docs/SMC-PROTOCOL.md` RD-08.

## Stack

- **Language:** Rust, edition 2021
- **Toolchain pin:** `rust-toolchain.toml` → channel `1.85.0` (matches `Cargo.toml` `rust-version = "1.85"`)
- **Dependencies:** exact-version pins per FR-078 (no caret, no tilde, no wildcards) — see `Cargo.toml`
- **Platform:** `aarch64-darwin` only (Apple Silicon M-series)
- **Privilege:** root via launchd; AppleSMC writes are safety-critical (see `docs/SMC-PROTOCOL.md` + `docs/SECURITY.md`)
- **Banned crates** (enforced by `deny.toml`): `tokio`, `async-std`, `smol`, `tracing`, `log`, `clap`, `openssl-sys`

## Repo layout

```
fand/
  Cargo.toml             # exact-pinned deps + clippy safety-critical lints
  rust-toolchain.toml    # 1.85.0 pin
  deny.toml              # cargo-deny supply-chain policy
  flake.nix              # aarch64-darwin-only flake (packages + module + devShell)
  build.rs               # build-time metadata
  src/
    main.rs              # entrypoint
    lib.rs               # library surface
    cli/                 # subcommand parsing (lexopt) — run, set, keys, curve, selftest, ...
    config/              # TOML schema + load + validate + reload
    control/             # curve eval, EMA, hysteresis, slew, panic, fusion, loop, state
    iohid/               # IOKit HID layer for sensor enumeration
    smc/                 # SMC FFI, write session, single-instance, signal handler, selftest
    ipc/                 # IPC socket + protocol
    launchd/             # LaunchDaemon / GCD integration
    log.rs               # bespoke logger (FR-097)
    security.rs          # security helpers
    correlation.rs       # request correlation IDs
  nix/
    package.nix          # rustPlatform build + ad-hoc codesign
    module.nix           # services.fand nix-darwin module
    sandbox-profiles/    # sandbox profile (fand-set.sb)
  benches/write_latency.rs
  tests/                 # integration, contract, property (proptest), golden, loom, miri-excluded FFI
  docs/                  # ARCHITECTURE, SECURITY, SMC-PROTOCOL, RESEARCH, schemas/
  scripts/               # sign-release, dev helpers
  supply-chain/          # cargo-vet audits.toml
  .github/workflows/     # ci, miri, nightly, no-ai-slips, osv-scanner, release, scorecard, sonar
```

## Run / build / test

```bash
nix build .#fand                       # build via flake (aarch64-darwin only)
cargo build --release                  # raw cargo build
cargo test --all-targets               # full test suite (no live hardware)
cargo test --features live-hardware    # real AppleSMC integration (requires root + Apple Silicon)
cargo bench --bench write_latency      # write-latency micro-benchmark (harness=false)
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
cargo deny check                       # supply-chain audit (advisories + licenses + bans + sources)
```

Nightly-only:

```bash
cargo +nightly miri test                              # UB / pointer provenance detector
RUSTFLAGS="--cfg loom" cargo +nightly test            # concurrency model checker (loom)
cargo test --features debug-watchdog-stall            # T053 watchdog-stall path
```

## Conventions

- Conventional Commits + DCO (`git commit -s`); no AI authorship trailers (enforced by the lefthook `commit-msg` guard)
- Worktree-first — branches named `NNN-slug`, worktree dirs end in `-NNN-slug`
- Release tags `vX.Y.Z` trigger `.github/workflows/release.yml` (never re-tag — cut `vX.Y.Z+1` on botched publish)
- Exact-pin dependency policy (FR-078). `cargo-deny` denies caret/tilde/wildcard + yanked + unmaintained + duplicates
- `cargo-vet` attestations in `supply-chain/audits.toml`; CVE rollback decision tree in `docs/SECURITY.md` §"Dependency rollback protocol"
- Safety-critical clippy set in `Cargo.toml` `[lints.clippy]`: `unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`, `indexing_slicing`, `dbg_macro`, `print_stdout` all `deny`

## Architecture

Single-threaded control loop, no async runtime. Per tick (default 500 ms):

1. Read sensor `flt ` values via SMC.
2. Reduce to driver temp (`max(sensors)` — hottest-of).
3. EMA smoothing + hysteresis applied inside curve eval.
4. Slew limit on ramp-down.
5. Panic-temp override + hold.
6. Write `F0md` (force-minimum vs auto) — Apple Silicon binary decision.

Module layout (`src/`):

- `cli/` — argv parsing (lexopt; `clap` is banned)
- `config/` — TOML load + schema validation + signal-driven reload
- `control/` — curve / ema / hysteresis / slew / fusion / panic / loop / state
- `iohid/` — IOKit HID sensor enumeration
- `smc/` — IOConnectCallStructMethod FFI, write session, single-instance flock (`fs4`), signal handler, sleep/wake, selftest
- `ipc/` — IPC socket + on-wire protocol
- `launchd/` — GCD integration + LaunchDaemon plist contract
- `log.rs` — bespoke logger (FR-097 bans `tracing` / `log`)

## Safety invariants (do not break)

- `F0md` writes are the only SMC writes in normal operation; arbitrary `Fx*` RPM keys are read-only on Apple Silicon (per `docs/SMC-PROTOCOL.md`).
- Always read `keyInfo.dataType` before decoding/encoding payloads — types differ from Intel (`flt ` not `sp78`/`fpe2`).
- Single-instance enforcement via flock (`fs4`); two daemons fighting the SMC is a thermal hazard. Re-eval slated `2026-10-01` per FR-101.
- Panic-temp path (`panic_temp_c` / `panic_hold_s`) must take precedence over curve output and survive sensor faults.
- No `tokio`/`async-std`/`smol`/`tracing`/`log`/`clap` — banned in `deny.toml`. The control loop is intentionally single-threaded.
- No `unwrap()`, `expect()`, `panic!()`, `todo!()`, `unreachable!()`, indexing slicing on the hot path — denied by clippy lints.

## CI workflows (`.github/workflows/`)

- `ci.yml` — build + test + lint + nix + cargo-deny + sonar (pre-merge)
- `miri.yml` — UB / pointer-provenance on the pure-logic subset (FR-083)
- `nightly.yml` — slow gates: loom, proptest deep runs, soak (FR-082, FR-085, FR-086)
- `osv-scanner.yml` — OSV-Scanner V2 with Rust call-graph reachability
- `scorecard.yml` — OpenSSF Scorecard
- `release.yml` — tag-triggered binary build + GitHub Release + provenance attestation (FR-022–025, FR-032, FR-044–047)
- `no-ai-slips.yml` — stealth + framing lint per vault Principle IX
- `sonar.yml` — SonarQube (self-hosted, project token scoped)

## Cross-references

- **Architecture deep-dive:** `docs/ARCHITECTURE.md`
- **Threat model + CVE protocol:** `docs/SECURITY.md` + `specs/005-smc-write-roundtrip/threat-model.md`
- **Hardware protocol reference:** `docs/SMC-PROTOCOL.md`
- **Research notes:** `docs/RESEARCH.md`
- **Sibling repos in portfolio:** `yolo-labz/wa`, `yolo-labz/claude-mac-chrome`, `yolo-labz/kokoro-speakd`, `yolo-labz/claude-classroom-submit`, `yolo-labz/portfolio`

## Release verification

```bash
gh attestation verify ./fand-aarch64-darwin --owner yolo-labz
```

Advanced / offline verification (cosign + slsa-verifier) lives in the release notes.

## License

MIT — see `LICENSE`.
