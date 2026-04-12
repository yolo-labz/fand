//! Property-based tests for sensor plausibility and fusion (T047, FR-100).
//!
//! Properties:
//! 1. Fusion rejects NaN/inf/negative — never returns NaN.
//! 2. Hold logic: all-dropout returns last_known_good, never NaN.
//! 3. Max-fusion output ≤ max of inputs (when valid inputs exist).

use proptest::prelude::*;

/// Mirror of the fusion::fuse function for property testing.
/// (fand is a binary crate — we re-implement the logic.)
fn fuse_max(values: &[f32], dropouts: &[bool], last_known_good: f32) -> f32 {
    let valid: Vec<f32> = values
        .iter()
        .zip(dropouts.iter())
        .filter(|(_, &d)| !d)
        .map(|(&v, _)| v)
        .collect();

    if valid.is_empty() {
        return last_known_good;
    }

    let result = valid.iter().copied().fold(f32::NEG_INFINITY, f32::max);

    if result.is_nan() || result.is_infinite() {
        return last_known_good;
    }
    result
}

/// Plausibility check mirror — FR-018.
fn is_plausible(val: f32) -> bool {
    val.is_finite() && val >= 0.0 && val <= 150.0
}

fn arb_sensor_values(n: usize) -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(
        prop_oneof![
            // Normal temperature range
            (0.0f32..150.0f32),
            // Edge cases
            Just(f32::NAN),
            Just(f32::INFINITY),
            Just(f32::NEG_INFINITY),
            Just(-10.0f32),
            Just(200.0f32),
            Just(0.0f32),
            Just(150.0f32),
        ],
        n,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10000))]

    #[test]
    fn fusion_never_returns_nan(
        values in arb_sensor_values(4),
        last_known_good in 20.0f32..100.0f32,
    ) {
        let dropouts: Vec<bool> = values.iter().map(|v| !is_plausible(*v)).collect();
        let result = fuse_max(&values, &dropouts, last_known_good);
        prop_assert!(!result.is_nan(), "fusion returned NaN for values={values:?}");
        prop_assert!(result.is_finite(), "fusion returned infinite for values={values:?}");
    }

    #[test]
    fn all_dropout_returns_last_known_good(
        last_known_good in 20.0f32..100.0f32,
    ) {
        let values = [f32::NAN, f32::INFINITY, -5.0, 200.0];
        let dropouts = [true, true, true, true];
        let result = fuse_max(&values, &dropouts, last_known_good);
        prop_assert!(
            (result - last_known_good).abs() < 0.01,
            "expected {last_known_good}, got {result}"
        );
    }

    #[test]
    fn max_output_bounded_by_inputs(
        values in arb_sensor_values(4),
        last_known_good in 20.0f32..100.0f32,
    ) {
        let dropouts: Vec<bool> = values.iter().map(|v| !is_plausible(*v)).collect();
        let has_valid = dropouts.iter().any(|&d| !d);
        if !has_valid {
            return Ok(());
        }
        let result = fuse_max(&values, &dropouts, last_known_good);
        let max_valid = values.iter()
            .zip(dropouts.iter())
            .filter(|(_, &d)| !d)
            .map(|(&v, _)| v)
            .fold(f32::NEG_INFINITY, f32::max);
        prop_assert!(
            result <= max_valid + 0.01,
            "result {result} > max_valid {max_valid}"
        );
    }

    #[test]
    fn plausibility_rejects_special_values(
        val in prop_oneof![Just(f32::NAN), Just(f32::INFINITY), Just(f32::NEG_INFINITY), Just(-1.0f32), Just(151.0f32)],
    ) {
        prop_assert!(!is_plausible(val), "should reject {val}");
    }

    #[test]
    fn plausibility_accepts_normal_range(
        val in 0.0f32..150.0f32,
    ) {
        prop_assert!(is_plausible(val), "should accept {val}");
    }
}
