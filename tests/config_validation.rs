//! Config validation edge-case tests (T048).
//!
//! These tests spawn `fand run --dry-run --once --config <tempfile>`
//! and check the exit code. They test the config parsing + validation
//! pipeline end-to-end via the binary's behavior.

use std::io::Write;
use std::process::Command;
use std::path::PathBuf;

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

fn run_with_config(content: &str) -> i32 {
    let mut f = tempfile::NamedTempFile::new().expect("create temp");
    f.write_all(content.as_bytes()).expect("write temp");
    let status = Command::new(fand_binary())
        .args(["run", "--dry-run", "--once", "--config"])
        .arg(f.path())
        .env("FAND_ALLOW_TMP_CONFIG", "1")
        .status()
        .expect("spawn fand");
    status.code().unwrap_or(-1)
}

#[test]
fn valid_minimal_config_exits_zero() {
    assert_eq!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_missing_fan_section() {
    assert_ne!(run_with_config("config_version = 1\n"), 0);
}

#[test]
fn reject_single_curve_point() {
    assert_ne!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317]]
"#), 0);
}

#[test]
fn reject_non_monotone_temps() {
    assert_ne!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[80.0, 6550], [50.0, 2317]]
"#), 0);
}

#[test]
fn reject_duplicate_fan_index() {
    assert_ne!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
[[fan]]
index = 0
sensors = ["Tf09"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_bad_poll_interval_low() {
    assert_ne!(run_with_config(r#"
config_version = 1
poll_interval_ms = 50
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_bad_poll_interval_high() {
    assert_ne!(run_with_config(r#"
config_version = 1
poll_interval_ms = 10000
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_empty_sensors() {
    assert_ne!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = []
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_bad_fourcc_length() {
    assert_ne!(run_with_config(r#"
config_version = 1
[[fan]]
index = 0
sensors = ["TooLong"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_unknown_fields() {
    assert_ne!(run_with_config(r#"
config_version = 1
unknown_field = "surprise"
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn reject_bad_config_version() {
    assert_ne!(run_with_config(r#"
config_version = 99
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn accept_boundary_poll_values() {
    assert_eq!(run_with_config(r#"
config_version = 1
poll_interval_ms = 100
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
    assert_eq!(run_with_config(r#"
config_version = 1
poll_interval_ms = 5000
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#), 0);
}

#[test]
fn missing_file_exits_one() {
    let status = Command::new(fand_binary())
        .args(["run", "--dry-run", "--once", "--config", "/no/such/file.toml"])
        .status()
        .expect("spawn fand");
    assert_eq!(status.code(), Some(1));
}
