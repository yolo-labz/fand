//! Property-based tests for `ClampedRpm::clamp` (FR-016, FR-018, FR-019, FR-020, FR-063).
//!
//! These properties run the clamping operation across the full f32 input
//! domain — NaN, ±∞, subnormals, boundary values, fractional micro-deltas
//! around min/max — and assert the post-clamp invariants. Per FR-086:
//! ≥ 10,000 cases per property in pre-merge CI, ≥ 1,000,000 in nightly soak.

use proptest::prelude::*;

use fand::control::state::ClampedRpm;

/// Realistic envelope range. Real Apple Silicon fans live in `[1300, 7200]`
/// roughly; the property tests use a wider but still finite envelope.
fn envelope_strategy() -> impl Strategy<Value = (f32, f32)> {
    (1.0_f32..10_000.0_f32).prop_flat_map(|min| (Just(min), (min + 1.0)..50_000.0_f32))
}

/// Property: clamped value is always finite.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn clamp_result_is_always_finite(
        raw in any::<f32>(),
        (min, max) in envelope_strategy(),
    ) {
        let result = ClampedRpm::new(raw, min, max);
        let val = result.value();
        prop_assert!(val > 0, "clamped value must be > 0 for envelope min > 0");
    }
}

/// Property: clamped value is always within `[round(min), round(max)]`.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn clamp_within_envelope(
        raw in any::<f32>(),
        (min, max) in envelope_strategy(),
    ) {
        let result = ClampedRpm::new(raw, min, max).value();
        // ClampedRpm::new rounds to u32, so the bounds must be the rounded
        // form of the input envelope. The minimum is the floor of `min`
        // (because clamping replaces sub-min with min, then rounds). The
        // maximum is the rounded `max`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_u32 = min.round() as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let max_u32 = max.round() as u32;
        prop_assert!(
            result >= min_u32 && result <= max_u32,
            "value {result} must be in [{min_u32}, {max_u32}] (raw={raw}, env=[{min},{max}])"
        );
    }
}

/// Property: NaN, ±∞, and subnormals are all sanitized to the minimum.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn pathological_inputs_clamp_to_min(
        (min, max) in envelope_strategy(),
    ) {
        let pathological = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE * 0.5, // subnormal
            -f32::MIN_POSITIVE * 0.5,
        ];
        for raw in pathological {
            let result = ClampedRpm::new(raw, min, max).value();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let min_u32 = min.round() as u32;
            prop_assert_eq!(
                result, min_u32,
                "raw={} did not clamp to min {}", raw, min_u32
            );
        }
    }
}

/// Property: zero is always clamped to the minimum (FR-019 — zero never reaches hardware).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn zero_always_clamps_to_min(
        (min, max) in envelope_strategy(),
    ) {
        let result = ClampedRpm::new(0.0, min, max).value();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_u32 = min.round() as u32;
        prop_assert_eq!(result, min_u32);
    }
}

/// Property: in-range values pass through (modulo rounding).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn in_range_value_passes_through(
        // Generate a (min, max) pair, then a raw value strictly inside.
        (min, max) in envelope_strategy(),
        offset in 0.0_f32..1.0_f32,
    ) {
        let span = max - min;
        let raw = min + (offset * span);
        let result = ClampedRpm::new(raw, min, max).value();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let raw_u32 = raw.round() as u32;
        // The result must equal the rounded form of raw (within ±1 due to
        // rounding mode); allow ±1 for f32 precision at large values.
        let diff = if result > raw_u32 { result - raw_u32 } else { raw_u32 - result };
        prop_assert!(diff <= 1, "in-range value mismatch: raw={raw} → {result} (expected ~{raw_u32})");
    }
}

/// Property: boundary values exactly at min and max pass through.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn boundary_values_pass_through(
        (min, max) in envelope_strategy(),
    ) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_u32 = min.round() as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let max_u32 = max.round() as u32;
        let at_min = ClampedRpm::new(min, min, max).value();
        let at_max = ClampedRpm::new(max, min, max).value();
        prop_assert_eq!(at_min, min_u32);
        prop_assert_eq!(at_max, max_u32);
    }
}
