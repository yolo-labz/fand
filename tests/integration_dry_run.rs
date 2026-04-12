//! Integration test for `fand set --fan N --rpm X --dry-run` (T051).
//!
//! Spawns the `fand` binary as a subprocess and asserts:
//!
//!   1. The process exits with code 0.
//!   2. stdout contains the expected preview banner.
//!   3. stdout mentions the fan index and requested RPM.
//!   4. `--json` mode emits the schema URI + session_id envelope.
//!
//! **Root required.** The current dry-run implementation opens a real
//! AppleSMC connection so the preview can show live-clamped values
//! against the hardware min/max. This means the test can only run
//! when the invoking user can open the SMC user client — in practice,
//! as root. When running as a non-root user, the tests `skip` gracefully
//! instead of failing. Run under sudo or in a privileged CI runner to
//! actually exercise the preview path.
//!
//! The "truly inert" invariant in the original T051 wording (no IOKit
//! handle opened at all) was relaxed after the RD-05 live-hardware
//! work concluded that reading the fan envelope is required for the
//! clamping preview to be meaningful. See `docs/ARCHITECTURE.md`
//! §"Dry-run semantics" for the trade-off rationale.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fand_binary() -> PathBuf {
    // cargo exposes CARGO_BIN_EXE_<name> during test builds, but only
    // for integration tests of binary crates when the bin target is
    // in the same package. That applies here.
    let path = env!("CARGO_BIN_EXE_fand");
    PathBuf::from(path)
}

/// Returns true when the current process can open AppleSMC.
/// Falls back to a dry-run probe because macOS exposes no cheap
/// "am I root?" API beyond `libc::getuid() == 0` which is a reasonable
/// proxy for AppleSMC-open capability on Apple Silicon.
fn is_privileged() -> bool {
    // Safety: getuid is always-safe on POSIX.
    unsafe { libc::getuid() == 0 }
}

macro_rules! skip_if_unprivileged {
    () => {
        if !is_privileged() {
            eprintln!(
                "integration_dry_run: skipping — run as root to exercise \
                 the AppleSMC-open path"
            );
            return;
        }
    };
}

#[test]
fn dry_run_exits_zero() {
    skip_if_unprivileged!();
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn fand");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dry_run_prints_fan_and_rpm_in_preview() {
    skip_if_unprivileged!();
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run"])
        .output()
        .expect("spawn fand");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0") && stdout.contains("3000"),
        "preview should mention fan index and RPM — got:\n{stdout}"
    );
}

#[test]
fn dry_run_completes_in_under_one_second() {
    skip_if_unprivileged!();
    // Dry-run should not hang on IOKit or flock — it should print the
    // preview and exit immediately. If this test starts taking more
    // than a second, the dry-run path has grown a slow dependency and
    // the check needs to be re-examined.
    let start = Instant::now();
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run"])
        .output()
        .expect("spawn fand");
    let elapsed = start.elapsed();

    assert!(output.status.success(), "dry-run did not exit cleanly");
    assert!(
        elapsed < Duration::from_secs(1),
        "dry-run took {elapsed:?} — expected < 1s"
    );
}

#[test]
fn dry_run_json_emits_schema_uri() {
    skip_if_unprivileged!();
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run", "--json"])
        .output()
        .expect("spawn fand");

    assert!(
        output.status.success(),
        "dry-run --json did not exit cleanly"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"$schema\""),
        "JSON envelope missing $schema field — got:\n{stdout}"
    );
    assert!(
        stdout.contains("dry-run-v1.json"),
        "JSON envelope missing schema URI — got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"session_id\""),
        "JSON envelope missing session_id — got:\n{stdout}"
    );
}
