//! Live-hardware integration tests (T052, T053, US1).
//!
//! Gated behind the `live-hardware` feature. Requires:
//!
//!   - Apple Silicon host (M1/M2/M3/M4/M5)
//!   - root privileges (AppleSMC open)
//!   - no concurrent fand process holding the `/var/run/fand-smc.lock`
//!
//! Run with:
//!
//!   sudo cargo test --features live-hardware --test live_smc_write -- --nocapture
//!
//! These tests actually move the fan. They complete in under 10 s per
//! test and always leave the fan in AUTO mode on exit (signal-thread
//! teardown). On macOS 15+ the thermal manager retains final authority
//! over the duty cycle — F0md=1 is an arbiter input, not an absolute
//! override.

#![cfg(feature = "live-hardware")]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

fn assert_root() {
    assert_eq!(
        unsafe { libc::getuid() },
        0,
        "live-hardware tests require root"
    );
}

/// T052: `fand set --commit` succeeds, converges, and SIGINT teardown
/// restores F0md=0 within the 500 ms FR-022 budget.
#[test]
fn fand_set_commit_then_sigint() {
    assert_root();

    let mut child = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "2317", "--commit"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fand");

    // Allow 2 s for Ftst fall-through + F0md=1 commit + convergence.
    sleep(Duration::from_secs(2));

    // Send SIGINT and start the 500 ms teardown timer.
    let pid = child.id();
    unsafe {
        libc::kill(pid as i32, libc::SIGINT);
    }

    let teardown_start = Instant::now();
    let status = child.wait().expect("wait fand");
    let teardown_elapsed = teardown_start.elapsed();

    assert!(
        teardown_elapsed < Duration::from_millis(500),
        "teardown took {teardown_elapsed:?} — exceeds 500 ms FR-022 budget"
    );
    assert!(
        status.success(),
        "fand should exit 0 on SIGINT, got {status:?}"
    );

    // Post-teardown verification: F0md must be 0 (auto). Probe with
    // `fand keys --read F0md` and inspect stdout.
    let probe = Command::new(fand_binary())
        .args(["keys", "--read", "F0md"])
        .output()
        .expect("probe fand keys --read F0md");
    let stdout = String::from_utf8_lossy(&probe.stdout);
    assert!(
        stdout.contains("=0") || stdout.contains(": 0") || stdout.contains("\"value\":0"),
        "F0md should read 0 (auto) post-teardown — got:\n{stdout}"
    );
}

/// T053: the FR-002 userspace watchdog fires and exits with code 4
/// when the heartbeat stalls for > 4 s.
///
/// Requires the `debug-watchdog-stall` feature which compiles in a
/// shorter watchdog deadline (e.g., 1 s) so the test doesn't have to
/// wait 4 real seconds.
#[test]
#[cfg(feature = "debug-watchdog-stall")]
fn fand_set_watchdog_fires() {
    assert_root();

    let start = Instant::now();
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "2317", "--commit"])
        .output()
        .expect("spawn fand");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "watchdog should fire within 5 s — took {elapsed:?}"
    );

    let code = output.status.code().expect("expected exit code");
    assert_eq!(
        code,
        4,
        "watchdog fire should exit with code 4 — got {code}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
