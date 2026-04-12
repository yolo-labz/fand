use fand::control::hysteresis::{apply, Direction, HysteresisState};

#[test]
fn rising_crosses_up_margin() {
    let mut s = HysteresisState::new(2000.0, 60.0);
    let rpm = apply(&mut s, 3000.0, 62.0, 1.0, 3.0);
    assert_eq!(rpm, 3000.0);
    assert_eq!(s.direction, Direction::Rising);
}

#[test]
fn held_within_band() {
    let mut s = HysteresisState::new(2000.0, 60.0);
    let rpm = apply(&mut s, 2200.0, 60.5, 1.0, 3.0);
    assert_eq!(rpm, 2000.0);
}

#[test]
fn falling_crosses_down_margin() {
    let mut s = HysteresisState::new(4000.0, 70.0);
    s.direction = Direction::Falling;
    let rpm = apply(&mut s, 2000.0, 66.0, 1.0, 3.0);
    assert_eq!(rpm, 2000.0);
    assert_eq!(s.direction, Direction::Falling);
}

#[test]
fn asymmetric_margins_hold_on_small_fall() {
    let mut s = HysteresisState::new(3000.0, 65.0);
    let rpm = apply(&mut s, 2800.0, 63.0, 1.0, 3.0);
    assert_eq!(rpm, 3000.0);
}

#[test]
fn direction_change_rising_to_falling() {
    let mut s = HysteresisState::new(3000.0, 65.0);
    s.direction = Direction::Rising;
    let rpm = apply(&mut s, 1500.0, 61.0, 1.0, 3.0);
    assert_eq!(rpm, 1500.0);
    assert_eq!(s.direction, Direction::Falling);
}

#[test]
fn reinit_after_bumpless() {
    let mut s = HysteresisState::new(5000.0, 80.0);
    s.direction = Direction::Rising;
    s.reinit(3000.0, 65.0);
    assert_eq!(s.hold_rpm, 3000.0);
    assert_eq!(s.direction, Direction::Held);
    assert_eq!(s.last_temp, 65.0);
}
