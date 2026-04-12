use fand::control::{curve, ema, fusion::FusionMode, fusion};

#[test]
fn golden_curve_evaluation() {
    let breakpoints = vec![(50.0, 0), (65.0, 2500), (80.0, 6000)];

    let cases: &[(f32, f32)] = &[
        (48.0, 0.0),
        (50.0, 0.0),
        (51.0, 166.67),
        (55.0, 833.33),
        (60.0, 1666.67),
        (65.0, 2500.0),
        (70.0, 3666.67),
        (75.0, 4833.33),
        (80.0, 6000.0),
        (85.0, 6000.0),
    ];

    for &(temp, expected) in cases {
        let rpm = curve::evaluate(&breakpoints, temp);
        assert!(
            (rpm - expected).abs() < 1.0,
            "temp={temp}: expected={expected}, got={rpm}"
        );
    }
}

#[test]
fn golden_fusion_max() {
    let cases: &[(&[f32], f32)] = &[
        (&[60.0, 72.5, 65.0], 72.5),
        (&[45.0, 45.0, 45.0], 45.0),
        (&[80.0, 60.0, 70.0], 80.0),
    ];
    for (vals, expected) in cases {
        let drops = vec![false; vals.len()];
        let result = fusion::fuse(vals, &drops, FusionMode::Max, 50.0);
        assert!(
            (result - expected).abs() < 0.01,
            "vals={vals:?}: expected={expected}, got={result}"
        );
    }
}

#[test]
fn golden_ema_sequence() {
    let mut smoothed = 70.0;
    let raw_sequence = [70.0, 65.0, 60.0, 55.0, 50.0];
    let expected = [70.0, 68.75, 66.5625, 63.672, 60.254];

    for (i, &raw) in raw_sequence.iter().enumerate() {
        smoothed = ema::smooth(smoothed, raw, 0.25);
        assert!(
            (smoothed - expected[i]).abs() < 0.01,
            "tick {i}: expected={}, got={smoothed}",
            expected[i]
        );
    }
}
