use fand::control::fusion::{fuse, FusionMode};

#[test]
fn max_of_three_sensors() {
    let vals = [60.0, 72.5, 65.0];
    let drops = [false, false, false];
    assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 72.5);
}

#[test]
fn mean_of_three_sensors() {
    let vals = [60.0, 72.0, 66.0];
    let drops = [false, false, false];
    let result = fuse(&vals, &drops, FusionMode::Mean, 50.0);
    assert!((result - 66.0).abs() < 0.01);
}

#[test]
fn single_sensor_max() {
    let vals = [71.3];
    let drops = [false];
    assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 71.3);
}

#[test]
fn all_dropout_returns_fallback() {
    let vals = [60.0, 72.5];
    let drops = [true, true];
    assert_eq!(fuse(&vals, &drops, FusionMode::Max, 42.0), 42.0);
}

#[test]
fn partial_dropout_excludes() {
    let vals = [60.0, 72.5, 65.0];
    let drops = [false, true, false];
    assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 65.0);
}

#[test]
fn empty_sensors_returns_fallback() {
    let vals: [f32; 0] = [];
    let drops: [bool; 0] = [];
    assert_eq!(fuse(&vals, &drops, FusionMode::Mean, 55.0), 55.0);
}
