use fand::control::ema;

#[test]
fn alpha_one_passthrough() {
    assert_eq!(ema::smooth(70.0, 45.0, 1.0), 45.0);
}

#[test]
fn alpha_quarter_blending() {
    let result = ema::smooth(70.0, 50.0, 0.25);
    let expected = 0.25 * 50.0 + 0.75 * 70.0;
    assert!((result - expected).abs() < 0.001);
}

#[test]
fn convergence_to_constant() {
    let mut s = 70.0;
    for _ in 0..200 {
        s = ema::smooth(s, 50.0, 0.25);
    }
    assert!((s - 50.0).abs() < 0.001);
}

#[test]
fn reinit_on_resume_is_passthrough() {
    let raw = 40.0;
    assert_eq!(ema::smooth(raw, raw, 0.25), 40.0);
}
