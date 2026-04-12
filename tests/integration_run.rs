//! Integration test for `fand run --once --dry-run` (T025, US1).
//!
//! Spawns the fand binary as a subprocess and asserts:
//!   1. Exit code 0.
//!   2. Stdout contains the expected per-tick output.
//!   3. Completes in under 2 seconds (SC-006).
//!   4. JSON mode emits valid JSONL.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

#[test]
fn dry_run_once_exits_zero() {
    let output = Command::new(fand_binary())
        .env("FAND_ALLOW_TMP_CONFIG", "1")
        .args(["run", "--config", "tests/fixtures/test-curve.toml", "--dry-run", "--once"])
        .output()
        .expect("spawn fand");
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn dry_run_once_prints_tick_output() {
    let output = Command::new(fand_binary())
        .env("FAND_ALLOW_TMP_CONFIG", "1")
        .args(["run", "--config", "tests/fixtures/test-curve.toml", "--dry-run", "--once"])
        .output()
        .expect("spawn fand");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tick 1"), "expected tick output, got: {stdout}");
    assert!(stdout.contains("fan 0"), "expected fan 0 in output");
    assert!(stdout.contains("RPM"), "expected RPM in output");
}

#[test]
fn dry_run_once_completes_in_under_two_seconds() {
    let start = Instant::now();
    let output = Command::new(fand_binary())
        .env("FAND_ALLOW_TMP_CONFIG", "1")
        .args(["run", "--config", "tests/fixtures/test-curve.toml", "--dry-run", "--once"])
        .output()
        .expect("spawn fand");
    let elapsed = start.elapsed();
    assert!(output.status.success());
    assert!(
        elapsed < Duration::from_secs(2),
        "dry-run --once took {elapsed:?} — expected < 2s (SC-006)"
    );
}

#[test]
fn dry_run_json_emits_valid_jsonl() {
    let output = Command::new(fand_binary())
        .args([
            "run", "--config", "tests/fixtures/test-curve.toml",
            "--dry-run", "--once", "--json",
        ])
        .output()
        .expect("spawn fand");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should be exactly one JSONL line.
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1, "expected 1 JSONL line, got {}", lines.len());
    // Should be valid JSON.
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .expect("JSONL line must be valid JSON");
    assert!(parsed.is_object());
    assert!(parsed["$schema"].is_string());
    assert!(parsed["session_id"].is_string());
    assert_eq!(parsed["schema_version"].as_u64(), Some(1));
    assert_eq!(parsed["tick_number"].as_u64(), Some(1));
}

#[test]
fn missing_config_exits_one() {
    let status = Command::new(fand_binary())
        .args(["run", "--config", "/nonexistent/fand.toml", "--dry-run", "--once"])
        .status()
        .expect("spawn fand");
    let code = status.code().expect("exit code");
    assert_eq!(code, 1, "missing config should exit 1");
}
