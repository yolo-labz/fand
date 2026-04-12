//! Property-based tests for curve interpolation (T046, FR-100).
//!
//! Properties:
//! 1. Interpolated RPM is always in [first_rpm, last_rpm].
//! 2. Monotonically increasing temps produce non-decreasing RPMs.
//! 3. Exact breakpoint temperatures return exact breakpoint RPMs.

use proptest::prelude::*;

/// The curve::evaluate function from src/control/curve.rs.
/// Re-implemented here because fand is a binary crate, not a library.
fn evaluate(breakpoints: &[(f32, u32)], temp: f32) -> f32 {
    let len = breakpoints.len();
    if len == 0 {
        return 0.0;
    }
    if len == 1 || temp <= breakpoints[0].0 {
        return breakpoints[0].1 as f32;
    }
    if temp >= breakpoints[len - 1].0 {
        return breakpoints[len - 1].1 as f32;
    }
    let mut i = 1;
    while i < len {
        if temp <= breakpoints[i].0 {
            let (t_lo, rpm_lo) = breakpoints[i - 1];
            let (t_hi, rpm_hi) = breakpoints[i];
            let dt = t_hi - t_lo;
            if dt.abs() < f32::EPSILON {
                return rpm_lo as f32;
            }
            let t = (temp - t_lo) / dt;
            let a = rpm_lo as f32;
            let b = rpm_hi as f32;
            return (1.0 - t) * a + t * b;
        }
        i += 1;
    }
    breakpoints[len - 1].1 as f32
}

fn arb_breakpoints() -> impl Strategy<Value = Vec<(f32, u32)>> {
    // Generate 2-10 breakpoints with strictly increasing temps.
    (2..=10usize)
        .prop_flat_map(|n| prop::collection::vec((0.0f32..150.0f32, 0u32..10000u32), n))
        .prop_map(|mut bps| {
            // Sort by temp and deduplicate.
            bps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            bps.dedup_by(|a, b| (a.0 - b.0).abs() < 0.1);
            if bps.len() < 2 {
                bps = vec![(40.0, 1000), (90.0, 6000)];
            }
            bps
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10000))]

    #[test]
    fn output_in_range(
        bps in arb_breakpoints(),
        temp in -10.0f32..160.0f32,
    ) {
        let rpm = evaluate(&bps, temp);
        // The output must be within [global_min_rpm, global_max_rpm]
        // across ALL breakpoints, not just first/last.
        let global_min = bps.iter().map(|b| b.1 as f32).fold(f32::INFINITY, f32::min);
        let global_max = bps.iter().map(|b| b.1 as f32).fold(f32::NEG_INFINITY, f32::max);
        prop_assert!(rpm >= global_min - 1.0, "rpm {rpm} < global_min {global_min}");
        prop_assert!(rpm <= global_max + 1.0, "rpm {rpm} > global_max {global_max}");
    }

    #[test]
    fn monotonic_input_monotonic_output(
        bps in arb_breakpoints(),
    ) {
        // If RPMs are also monotonically non-decreasing in breakpoints...
        let rpms_monotone = bps.windows(2).all(|w| w[1].1 >= w[0].1);
        if !rpms_monotone {
            return Ok(()); // Skip non-monotone RPM curves.
        }
        // Then increasing temps should produce non-decreasing RPMs.
        let mut prev_rpm = 0.0f32;
        for i in 0..100 {
            let t = bps[0].0 + (bps[bps.len() - 1].0 - bps[0].0) * i as f32 / 99.0;
            let rpm = evaluate(&bps, t);
            prop_assert!(rpm >= prev_rpm - 1.0, "non-monotone at t={t}: {rpm} < prev {prev_rpm}");
            prev_rpm = rpm;
        }
    }

    #[test]
    fn exact_breakpoint_returns_exact_rpm(
        bps in arb_breakpoints(),
    ) {
        for &(temp, rpm) in &bps {
            let result = evaluate(&bps, temp);
            prop_assert!((result - rpm as f32).abs() < 1.0,
                "at breakpoint temp={temp}, expected {rpm}, got {result}");
        }
    }
}
