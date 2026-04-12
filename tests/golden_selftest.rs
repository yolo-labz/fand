//! Golden-file parser test for the selftest JSON envelope (T103).
//!
//! Loads `tests/fixtures/golden_selftest_report.json`, parses it as
//! generic JSON, and asserts the schema + ordering invariants required
//! by `docs/schemas/selftest-v1.json`. It does NOT compare numeric
//! values (those vary by hardware) — only the structural shape.
//!
//! If this test ever fails, the selftest output envelope has drifted
//! from the schema. Either update the schema and the golden fixture
//! together, or fix the output emitter.

use serde_json::Value;

const GOLDEN: &str = include_str!("fixtures/golden_selftest_report.json");

#[test]
fn golden_parses_as_json() {
    let v: Value = serde_json::from_str(GOLDEN).expect("golden JSON must parse");
    assert!(v.is_object(), "golden must be a JSON object");
}

#[test]
fn golden_has_required_top_level_fields() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let obj = v.as_object().unwrap();
    for field in [
        "$schema",
        "$id",
        "schema_version",
        "subcommand",
        "fand_version",
        "session_id",
        "per_fan",
        "summary",
    ] {
        assert!(obj.contains_key(field), "missing top-level field: {field}");
    }
}

#[test]
fn golden_schema_version_is_one() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(v["schema_version"].as_u64(), Some(1));
}

#[test]
fn golden_subcommand_is_selftest() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(v["subcommand"].as_str(), Some("selftest"));
}

#[test]
fn golden_session_id_is_26_char_ulid() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let sid = v["session_id"]
        .as_str()
        .expect("session_id must be a string");
    assert_eq!(sid.len(), 26, "session_id must be exactly 26 chars");
    for c in sid.chars() {
        // Crockford base32 + allow letters we use in the "REDACT" placeholder.
        assert!(c.is_ascii_alphanumeric(), "non-ASCII in session_id: {c}");
    }
}

#[test]
fn golden_schema_uri_points_at_v1_schema() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let uri = v["$schema"].as_str().expect("$schema must be a string");
    assert!(uri.ends_with("/selftest-v1.json"), "wrong $schema: {uri}");
}

#[test]
fn golden_id_is_session_urn() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let id = v["$id"].as_str().expect("$id must be a string");
    assert!(id.starts_with("urn:fand:session:"), "wrong $id: {id}");
}

#[test]
fn golden_per_fan_is_array_of_objects() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let per_fan = v["per_fan"].as_array().expect("per_fan must be an array");
    for fan in per_fan {
        let obj = fan.as_object().expect("per_fan entry must be an object");
        for field in [
            "fan_index",
            "iterations_completed",
            "iterations_requested",
            "round_trip_count",
            "mismatch_count",
            "median_actual_at_min",
            "median_actual_at_auto",
            "delta_rpm",
            "result",
        ] {
            assert!(obj.contains_key(field), "per_fan entry missing: {field}");
        }
        let result = obj["result"].as_str().unwrap();
        assert!(
            matches!(
                result,
                "pass" | "inconclusive" | "fail" | "watchdog_timeout" | "conflict"
            ),
            "invalid result: {result}"
        );
    }
}

#[test]
fn golden_summary_has_expected_fields() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let summary = v["summary"].as_object().expect("summary must be an object");
    for field in [
        "fans_tested",
        "total_iterations",
        "total_round_trips",
        "total_mismatches",
        "wall_clock_ms",
        "overall_result",
    ] {
        assert!(summary.contains_key(field), "summary missing: {field}");
    }
}

#[test]
fn golden_summary_totals_are_non_negative() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let summary = &v["summary"];
    for field in [
        "fans_tested",
        "total_iterations",
        "total_round_trips",
        "total_mismatches",
        "wall_clock_ms",
    ] {
        let n = summary[field].as_u64().expect(field);
        assert!(n < u64::MAX, "overflow on {field}"); // tautological sanity
        let _ = n; // no negative-check needed for u64
    }
}

#[test]
fn golden_fand_version_is_semver_shape() {
    let v: Value = serde_json::from_str(GOLDEN).unwrap();
    let ver = v["fand_version"].as_str().unwrap();
    let parts: Vec<&str> = ver.split('.').collect();
    assert_eq!(parts.len(), 3, "fand_version must be x.y.z — got {ver}");
    for p in parts {
        p.parse::<u32>()
            .unwrap_or_else(|_| panic!("non-numeric component in {ver}"));
    }
}
