pub fn limit(prev_rpm: f32, target_rpm: f32, ramp_down_rpm_per_s: f32, actual_dt_secs: f32) -> f32 {
    if target_rpm >= prev_rpm {
        return target_rpm;
    }
    let clamped_dt = actual_dt_secs.min(1.0);
    let max_down = ramp_down_rpm_per_s * clamped_dt;
    let floor = prev_rpm - max_down;
    target_rpm.max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_up_unlimited() {
        assert_eq!(limit(1000.0, 5000.0, 600.0, 0.5), 5000.0);
    }

    #[test]
    fn ramp_down_capped() {
        let result = limit(5000.0, 1000.0, 600.0, 0.5);
        assert!((result - 4700.0).abs() < 0.01);
    }

    #[test]
    fn zero_delta() {
        assert_eq!(limit(3000.0, 3000.0, 600.0, 0.5), 3000.0);
    }

    #[test]
    fn actual_dt_based() {
        let result_short = limit(5000.0, 1000.0, 600.0, 0.3);
        let result_long = limit(5000.0, 1000.0, 600.0, 0.6);
        assert!(result_short > result_long);
    }

    #[test]
    fn dt_sanity_clamp() {
        let result = limit(5000.0, 1000.0, 600.0, 5.0);
        let expected = limit(5000.0, 1000.0, 600.0, 1.0);
        assert_eq!(result, expected);
    }

    #[test]
    fn seed_from_actual() {
        let actual_rpm = 3200.0;
        let curve_rpm = 2800.0;
        let result = limit(actual_rpm, curve_rpm, 600.0, 0.5);
        assert!((result - 2900.0).abs() < 0.01);
    }
}
