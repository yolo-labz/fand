use fand::control::{curve, ema, fusion, fusion::FusionMode, slew, state::ClampedRpm};

#[test]
fn output_always_in_bounds() {
    let min = 1300.0_f32;
    let max = 6400.0_f32;
    let breakpoints = vec![(50.0, 0), (65.0, 2500), (80.0, 6000)];

    for temp_x10 in 0..1200 {
        #[allow(clippy::cast_precision_loss)]
        let temp = temp_x10 as f32 / 10.0;
        let rpm = curve::evaluate(&breakpoints, temp);
        let clamped = ClampedRpm::new(rpm, min, max);
        assert!(
            clamped.value() >= min as u32 && clamped.value() <= max as u32,
            "temp={temp}, rpm={rpm}, clamped={} outside [{min}, {max}]",
            clamped.value()
        );
    }
}

#[test]
fn curve_monotone_nondecreasing() {
    let breakpoints = vec![(50.0, 0), (60.0, 1500), (70.0, 3000), (80.0, 6000)];

    let mut prev_rpm = f32::NEG_INFINITY;
    for temp_x10 in 0..1200 {
        #[allow(clippy::cast_precision_loss)]
        let temp = temp_x10 as f32 / 10.0;
        let rpm = curve::evaluate(&breakpoints, temp);
        assert!(
            rpm >= prev_rpm,
            "monotonicity violated: temp={temp}, rpm={rpm} < prev={prev_rpm}"
        );
        prev_rpm = rpm;
    }
}

#[test]
fn convergence_under_constant_temp() {
    let breakpoints = vec![(50.0, 0), (65.0, 2500), (80.0, 6000)];
    let constant_temp = 70.0;
    let target_rpm = curve::evaluate(&breakpoints, constant_temp);

    let mut smoothed = constant_temp;
    for _ in 0..20 {
        smoothed = ema::smooth(smoothed, constant_temp, 0.25);
    }
    let converged_rpm = curve::evaluate(&breakpoints, smoothed);
    assert!(
        (converged_rpm - target_rpm).abs() < 1.0,
        "did not converge: target={target_rpm}, got={converged_rpm}"
    );
}

#[test]
fn slew_never_exceeds_max_down() {
    let ramp_down = 600.0;
    let dt = 0.5;
    let max_down = ramp_down * dt;

    let mut prev = 6000.0;
    for step in 0..100 {
        let target = 1000.0;
        let result = slew::limit(prev, target, ramp_down, dt);
        if result < prev {
            let drop = prev - result;
            assert!(
                drop <= max_down + 0.01,
                "step {step}: drop={drop} > max_down={max_down}"
            );
        }
        prev = result;
    }
}

#[test]
fn fusion_nan_guard_property() {
    for _ in 0..100 {
        let result = fusion::fuse(&[], &[], FusionMode::Max, 55.0);
        assert!(!result.is_nan());
        assert!(!result.is_infinite());
    }
}

#[test]
fn clamped_rpm_idempotent() {
    let min = 1300.0;
    let max = 6400.0;
    for rpm_x10 in 0..100_000 {
        #[allow(clippy::cast_precision_loss)]
        let raw = rpm_x10 as f32 / 10.0;
        let first = ClampedRpm::new(raw, min, max);
        #[allow(clippy::cast_precision_loss)]
        let second = ClampedRpm::new(first.value() as f32, min, max);
        assert_eq!(first.value(), second.value());
    }
}
