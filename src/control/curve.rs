pub fn evaluate(breakpoints: &[(f32, u32)], temp: f32) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_first() {
        let bp = vec![(50.0, 0), (80.0, 6000)];
        assert!((evaluate(&bp, 30.0) - 0.0).abs() < 0.01);
    }

    #[test]
    fn above_last() {
        let bp = vec![(50.0, 0), (80.0, 6000)];
        assert!((evaluate(&bp, 100.0) - 6000.0).abs() < 0.01);
    }

    #[test]
    fn midpoint() {
        let bp = vec![(50.0, 0), (80.0, 6000)];
        assert!((evaluate(&bp, 65.0) - 3000.0).abs() < 1.0);
    }

    #[test]
    fn on_breakpoint() {
        let bp = vec![(50.0, 1000), (65.0, 2500), (80.0, 6000)];
        assert!((evaluate(&bp, 65.0) - 2500.0).abs() < 0.01);
    }

    #[test]
    fn endpoint_exactness_t0() {
        let bp = vec![(50.0, 1000), (80.0, 6000)];
        assert_eq!(evaluate(&bp, 50.0), 1000.0);
    }

    #[test]
    fn endpoint_exactness_t1() {
        let bp = vec![(50.0, 1000), (80.0, 6000)];
        assert_eq!(evaluate(&bp, 80.0), 6000.0);
    }

    #[test]
    fn zero_width_breakpoints() {
        let bp = vec![(65.0, 1000), (65.0, 5000)];
        assert_eq!(evaluate(&bp, 65.0), 1000.0);
    }

    #[test]
    fn two_breakpoint_minimum() {
        let bp = vec![(40.0, 1300), (90.0, 6400)];
        let rpm = evaluate(&bp, 65.0);
        let expected = 1300.0 + (6400.0 - 1300.0) * (65.0 - 40.0) / (90.0 - 40.0);
        assert!((rpm - expected).abs() < 1.0);
    }
}
