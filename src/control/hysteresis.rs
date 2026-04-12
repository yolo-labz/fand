#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Rising,
    Falling,
    Held,
}

#[derive(Debug, Clone)]
pub struct HysteresisState {
    pub hold_rpm: f32,
    pub direction: Direction,
    pub last_temp: f32,
}

impl HysteresisState {
    #[must_use]
    pub fn new(initial_rpm: f32, initial_temp: f32) -> Self {
        Self {
            hold_rpm: initial_rpm,
            direction: Direction::Held,
            last_temp: initial_temp,
        }
    }

    pub fn reinit(&mut self, rpm: f32, temp: f32) {
        self.hold_rpm = rpm;
        self.direction = Direction::Held;
        self.last_temp = temp;
    }
}

pub fn apply(
    state: &mut HysteresisState,
    curve_rpm: f32,
    smoothed_temp: f32,
    up_margin: f32,
    down_margin: f32,
) -> f32 {
    let delta = smoothed_temp - state.last_temp;

    let should_emit = match state.direction {
        Direction::Rising | Direction::Held => {
            if delta > up_margin {
                true
            } else if delta < -down_margin {
                true
            } else {
                false
            }
        }
        Direction::Falling => {
            if delta < -down_margin {
                true
            } else if delta > up_margin {
                true
            } else {
                false
            }
        }
    };

    if should_emit {
        state.hold_rpm = curve_rpm;
        state.last_temp = smoothed_temp;
        state.direction = if delta > 0.0 {
            Direction::Rising
        } else if delta < 0.0 {
            Direction::Falling
        } else {
            Direction::Held
        };
    }

    state.hold_rpm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rising_crosses_up() {
        let mut s = HysteresisState::new(2000.0, 60.0);
        let rpm = apply(&mut s, 3000.0, 62.0, 1.0, 3.0);
        assert_eq!(rpm, 3000.0);
        assert_eq!(s.direction, Direction::Rising);
    }

    #[test]
    fn held_in_band() {
        let mut s = HysteresisState::new(2000.0, 60.0);
        let rpm = apply(&mut s, 2100.0, 60.5, 1.0, 3.0);
        assert_eq!(rpm, 2000.0);
        assert_eq!(s.direction, Direction::Held);
    }

    #[test]
    fn falling_crosses_down() {
        let mut s = HysteresisState::new(3000.0, 70.0);
        s.direction = Direction::Falling;
        let rpm = apply(&mut s, 1500.0, 66.0, 1.0, 3.0);
        assert_eq!(rpm, 1500.0);
        assert_eq!(s.direction, Direction::Falling);
    }

    #[test]
    fn asymmetric_margins() {
        let mut s = HysteresisState::new(2000.0, 60.0);
        let rpm_small_rise = apply(&mut s, 2200.0, 60.8, 1.0, 3.0);
        assert_eq!(rpm_small_rise, 2000.0);

        let rpm_small_fall = apply(&mut s, 1800.0, 58.0, 1.0, 3.0);
        assert_eq!(rpm_small_fall, 2000.0);

        let rpm_big_fall = apply(&mut s, 1500.0, 56.5, 1.0, 3.0);
        assert_eq!(rpm_big_fall, 1500.0);
    }

    #[test]
    fn reinit_resets() {
        let mut s = HysteresisState::new(5000.0, 80.0);
        s.direction = Direction::Rising;
        s.reinit(3000.0, 65.0);
        assert_eq!(s.hold_rpm, 3000.0);
        assert_eq!(s.direction, Direction::Held);
        assert_eq!(s.last_temp, 65.0);
    }
}
