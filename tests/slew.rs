use fand::control::slew;

#[test]
fn unlimited_ramp_up() {
    assert_eq!(slew::limit(1000.0, 6000.0, 600.0, 0.5), 6000.0);
}

#[test]
fn capped_ramp_down() {
    let result = slew::limit(5000.0, 1000.0, 600.0, 0.5);
    assert!((result - 4700.0).abs() < 0.01);
}

#[test]
fn zero_change() {
    assert_eq!(slew::limit(3000.0, 3000.0, 600.0, 0.5), 3000.0);
}

#[test]
fn large_delta_still_capped() {
    let result = slew::limit(6000.0, 0.0, 600.0, 0.5);
    assert!((result - 5700.0).abs() < 0.01);
}

#[test]
fn bumpless_seed_from_actual() {
    let actual = 3200.0;
    let curve = 2800.0;
    let result = slew::limit(actual, curve, 600.0, 0.5);
    assert!(result >= 2900.0);
    assert!(result <= 3200.0);
}

#[test]
fn actual_dt_varies_ramp() {
    let short_dt = slew::limit(5000.0, 3000.0, 600.0, 0.3);
    let long_dt = slew::limit(5000.0, 3000.0, 600.0, 0.7);
    assert!(short_dt > long_dt);
}

#[test]
fn dt_sanity_clamp_at_1s() {
    let clamped = slew::limit(5000.0, 0.0, 600.0, 10.0);
    let at_1s = slew::limit(5000.0, 0.0, 600.0, 1.0);
    assert_eq!(clamped, at_1s);
}
