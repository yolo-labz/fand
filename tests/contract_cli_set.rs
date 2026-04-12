//! Contract test for `fand set --dry-run --json` output schema (T049, US1).
//!
//! This asserts that the fand binary's JSON envelope conforms to the
//! contract defined in `specs/005-smc-write-roundtrip/contracts/cli-set.md §5`:
//!
//!   - top-level fields: schema_version, $schema, $id, fand_version,
//!     host, fan, request, planned_writes, planned_teardown
//!   - `schema_version` is 1
//!   - `$schema` points at docs/schemas/dry-run-v1.json
//!   - `$id` is a `urn:fand:session:<ULID>` URN
//!   - `planned_writes` is an array
//!   - `planned_teardown` is an array
//!
//! Requires root to exercise the full dry-run path (same caveat as
//! `integration_dry_run.rs`). Skips gracefully when non-privileged.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn fand_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fand"))
}

fn is_privileged() -> bool {
    unsafe { libc::getuid() == 0 }
}

fn run_dry_run_json() -> Option<Value> {
    if !is_privileged() {
        eprintln!("contract_cli_set: skipping — run as root");
        return None;
    }
    let output = Command::new(fand_binary())
        .args(["set", "--fan", "0", "--rpm", "3000", "--dry-run", "--json"])
        .output()
        .expect("spawn fand");
    if !output.status.success() {
        panic!(
            "fand set --dry-run --json failed: {:?}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).unwrap();
    Some(serde_json::from_str(&stdout).expect("JSON envelope must parse"))
}

#[test]
fn envelope_has_all_required_fields() {
    let Some(v) = run_dry_run_json() else { return };
    let obj = v.as_object().expect("envelope must be an object");
    for field in [
        "schema_version",
        "$schema",
        "$id",
        "fand_version",
        "session_id",
        "fan",
        "request",
        "planned_writes",
        "planned_teardown",
    ] {
        assert!(obj.contains_key(field), "missing field: {field}");
    }
}

#[test]
fn schema_version_is_one() {
    let Some(v) = run_dry_run_json() else { return };
    assert_eq!(v["schema_version"].as_u64(), Some(1));
}

#[test]
fn schema_uri_points_at_v1_schema() {
    let Some(v) = run_dry_run_json() else { return };
    let uri = v["$schema"].as_str().expect("$schema must be string");
    assert!(
        uri.ends_with("/dry-run-v1.json"),
        "wrong $schema URI: {uri}"
    );
}

#[test]
fn id_is_session_urn() {
    let Some(v) = run_dry_run_json() else { return };
    let id = v["$id"].as_str().expect("$id must be string");
    assert!(id.starts_with("urn:fand:session:"), "wrong $id URN: {id}");
}

#[test]
fn session_id_is_26_char_ulid() {
    let Some(v) = run_dry_run_json() else { return };
    let sid = v["session_id"].as_str().expect("session_id must be string");
    assert_eq!(sid.len(), 26, "session_id must be 26 chars");
    for c in sid.chars() {
        assert!(
            c.is_ascii_alphanumeric(),
            "non-base32 char in session_id: {c}"
        );
    }
}

#[test]
fn planned_writes_is_array() {
    let Some(v) = run_dry_run_json() else { return };
    assert!(
        v["planned_writes"].is_array(),
        "planned_writes must be array"
    );
}

#[test]
fn planned_teardown_is_array() {
    let Some(v) = run_dry_run_json() else { return };
    assert!(
        v["planned_teardown"].is_array(),
        "planned_teardown must be array"
    );
}

#[test]
fn fand_version_matches_cargo_pkg_version() {
    let Some(v) = run_dry_run_json() else { return };
    let ver = v["fand_version"]
        .as_str()
        .expect("fand_version must be string");
    assert_eq!(ver, env!("CARGO_PKG_VERSION"));
}
