# Fan demo provenance and read-only capture plan

Checked 21/09/2026 at base `9b72d0a`; documentation by OpenAI. No workflows,
security configuration, running daemon or hardware settings were changed.

`assets/fand-demo.cast` entered history in `989af53` (marketing PR #20).
It has 100×32 terminal cells and a header timestamp of 1748390400
(28/05/2025 UTC). Neither that self-declared timestamp nor the marketing commit
establishes the provenance of the displayed sensor readings. No host/build/raw
telemetry receipt accompanies it. The original bytes remain for history, but the
README no longer promotes them as a live recording or current measurement.

The current `src/cli/` contains `status`, `show` and `reload` commands. Existence
of those commands does not validate the cast's rendered values. The AppleSMC
implementation and `docs/SMC-PROTOCOL.md` require supported Apple Silicon hardware;
a Linux-generated mock cannot demonstrate actual thermal control.

## Future capture — read-only, gated on a suitable Mac

1. Use an isolated terminal with a blank prompt and no personal history. Record
   macOS version, Apple Silicon model, fand version and source revision, without
   serial numbers, usernames, private paths or other identifiers.
2. Record `fand --help` and read-only `fand status` from an **already running**
   daemon. Keep raw output and command exit codes. If no daemon is already running,
   report that blocker; do not start one solely for a recording.
3. Audit `fand show` output before including it: a live config may contain paths
   or machine-specific identifiers. Prefer a checked-in synthetic config for
   schema explanation, clearly labeled as a fixture rather than telemetry.
4. Do not execute `set`, `run`, `reload`, `selftest`, launchctl operations, SMC
   writes or thermal load generation for promotional media. Do not copy the
   legacy cast's `sudo fand reload` into a capture script.
5. Save actual cast/video plus exact commands, revision, dimensions, duration,
   SHA-256 hashes and a note that sensor values vary. No soundtrack is needed.

No isolated Mac capture was established during this Linux-only slice. Hardware
recording remains unfulfilled; documentation alone does not close that requirement.
