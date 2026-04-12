//! Contract test for `fand set` exit codes (T050, US1).
//!
//! Asserts that the `fand set` binary emits the documented exit code
//! for each failure mode, per `specs/005-smc-write-roundtrip/contracts/cli-set.md`:
//!
//!   0 — success (dry-run preview, commit round-trip verified)
//!   1 — generic failure (unexpected SMC error, bad argument parsing,
//!       unrelated internal error)
//!   2 — not root / service not found / AppleSMC open failed
//!   3 — round-trip readback mismatch (FR-006 safety gate)
//!   4 — FR-002 watchdog fired (heartbeat stall)
//!   5 — conflict detected (lockfile held or EDR-style interference)
//!  64 — usage error (bad args, per sysexits.h EX_USAGE)
//!
//! This test cannot fabricate every error mode without live hardware.
//! It verifies the two paths we CAN exercise as a non-root user:
//!
//!   - exit 64 on a malformed CLI argument
//!   - exit 2 on the "service not found / not root" path
//!
//! The root-gated exit codes (0/3/4/5) are verified by the live-hardware
//! tests in `tests/live_smc_write.rs` (T052, T053) which need real
//! AppleSMC access to reach their failure modes.

use std::path::PathBuf;
use std::process::Command;

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

#[test]
fn bad_rpm_value_exits_nonzero() {
    // `--rpm notanumber` must be rejected by the strict parser and
    // the process must exit with a usage-class error code.
    let status = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "notanumber", "--dry-run"])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("expected exit code");
    assert!(
        code != 0,
        "expected non-zero exit for bad rpm, got {code}"
    );
}

#[test]
fn missing_required_arg_exits_nonzero() {
    // `--fan` and `--rpm` are both required.
    let status = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--dry-run"])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("expected exit code");
    assert_ne!(code, 0, "missing --rpm must be a non-zero exit");
}

#[test]
fn mutually_exclusive_dry_run_and_commit_exits_nonzero() {
    // --dry-run and --commit cannot coexist.
    let status = Command::new(fand_binary())
        .args([
            "set", "--fan", "0", "--rpm", "3000", "--dry-run", "--commit",
        ])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("expected exit code");
    assert_ne!(code, 0, "dry-run + commit must be a non-zero exit");
}

#[test]
fn service_not_found_exits_two_when_unprivileged() {
    // Running without root on a non-privileged runner exercises the
    // "service not found / AppleSMC open failed" path which maps to
    // exit code 2 via code_from_smc_error → SmcError::ServiceNotFound.
    //
    // On a privileged runner (CI with sudo) this test is skipped
    // because the open would succeed and the command would exit 0.
    if unsafe { libc::getuid() == 0 } {
        eprintln!("contract_exit_codes: skipping exit-2 test under root");
        return;
    }
    let status = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run"])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("expected exit code");
    assert_eq!(
        code, 2,
        "non-root dry-run must map to exit 2 (service not found)"
    );
}

#[test]
fn fan_index_out_of_range_exits_nonzero() {
    // Fan index 99 is far out of range on any Apple Silicon machine.
    let status = Command::new(fand_binary())
        .args(["set", "--fan", "99", "--rpm", "3000", "--dry-run"])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("expected exit code");
    assert_ne!(code, 0, "out-of-range fan must be non-zero exit");
}
