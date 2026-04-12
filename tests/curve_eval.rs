use fand::control::curve;

#[test]
fn below_first_breakpoint() {
    let bp = vec![(50.0, 0), (65.0, 2500), (80.0, 6000)];
    assert_eq!(curve::evaluate(&bp, 30.0), 0.0);
}

#[test]
fn above_last_breakpoint() {
    let bp = vec![(50.0, 0), (65.0, 2500), (80.0, 6000)];
    assert_eq!(curve::evaluate(&bp, 100.0), 6000.0);
}

#[test]
fn between_two_breakpoints() {
    let bp = vec![(50.0, 0), (80.0, 6000)];
    let rpm = curve::evaluate(&bp, 65.0);
    assert!((rpm - 3000.0).abs() < 1.0);
}

#[test]
fn exactly_on_breakpoint() {
    let bp = vec![(50.0, 1000), (65.0, 2500), (80.0, 6000)];
    assert!((curve::evaluate(&bp, 65.0) - 2500.0).abs() < 0.01);
}

#[test]
fn two_breakpoint_minimum() {
    let bp = vec![(40.0, 1300), (90.0, 6400)];
    let rpm = curve::evaluate(&bp, 40.0);
    assert_eq!(rpm, 1300.0);
    let rpm2 = curve::evaluate(&bp, 90.0);
    assert_eq!(rpm2, 6400.0);
}

#[test]
fn zero_width_breakpoints_div_guard() {
    let bp = vec![(65.0, 1000), (65.0, 5000)];
    let rpm = curve::evaluate(&bp, 65.0);
    assert_eq!(rpm, 1000.0);
}

#[test]
fn close_breakpoints_precision() {
    let bp = vec![(64.99, 2000), (65.01, 6000)];
    let rpm = curve::evaluate(&bp, 65.0);
    assert!((rpm - 4000.0).abs() < 50.0);
}

#[test]
fn endpoint_exactness_t0_and_t1() {
    let bp = vec![(50.0, 1234), (80.0, 5678)];
    assert_eq!(curve::evaluate(&bp, 50.0), 1234.0);
    assert_eq!(curve::evaluate(&bp, 80.0), 5678.0);
}

#[test]
fn empty_breakpoints() {
    assert_eq!(curve::evaluate(&[], 50.0), 0.0);
}

#[test]
fn single_breakpoint() {
    let bp = vec![(60.0, 3000)];
    assert_eq!(curve::evaluate(&bp, 50.0), 3000.0);
    assert_eq!(curve::evaluate(&bp, 70.0), 3000.0);
}

#[test]
fn three_segment_interpolation() {
    let bp = vec![(50.0, 0), (60.0, 2000), (70.0, 4000), (80.0, 6000)];
    let rpm = curve::evaluate(&bp, 55.0);
    assert!((rpm - 1000.0).abs() < 1.0);
    let rpm2 = curve::evaluate(&bp, 75.0);
    assert!((rpm2 - 5000.0).abs() < 1.0);
}
