//! Integration test for `fand curve` (T036, US3).
//!
//! Spawns `fand curve --config test-curve.toml --fan 0` as a subprocess
//! and asserts the ASCII plot renders correctly. No root needed.

use std::path::PathBuf;
use std::process::Command;

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

#[test]
fn curve_exits_zero_with_valid_config() {
    let output = Command::new(fand_binary())
        .args(["curve", "--config", "tests/fixtures/test-curve.toml", "--fan", "0"])
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
fn curve_output_contains_fan_header() {
    let output = Command::new(fand_binary())
        .args(["curve", "--config", "tests/fixtures/test-curve.toml", "--fan", "0"])
        .output()
        .expect("spawn fand");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Fan 0"), "expected 'Fan 0' header, got:\n{stdout}");
    assert!(stdout.contains("hysteresis"), "expected hysteresis info");
}

#[test]
fn curve_output_contains_breakpoints() {
    let output = Command::new(fand_binary())
        .args(["curve", "--config", "tests/fixtures/test-curve.toml", "--fan", "0"])
        .output()
        .expect("spawn fand");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2317 RPM"), "expected 2317 RPM in curve points");
    assert!(stdout.contains("6550 RPM"), "expected 6550 RPM in curve points");
    assert!(stdout.contains("50.0"), "expected 50.0°C in curve points");
    assert!(stdout.contains("85.0"), "expected 85.0°C in curve points");
}

#[test]
fn curve_missing_config_exits_one() {
    let status = Command::new(fand_binary())
        .args(["curve", "--config", "/nonexistent.toml", "--fan", "0"])
        .status()
        .expect("spawn fand");
    assert_eq!(status.code(), Some(1));
}

#[test]
fn curve_bad_fan_index_exits_one() {
    let status = Command::new(fand_binary())
        .args(["curve", "--config", "tests/fixtures/test-curve.toml", "--fan", "99"])
        .status()
        .expect("spawn fand");
    assert_eq!(status.code(), Some(1));
}
