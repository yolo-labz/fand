pub fn smooth(prev_smoothed: f32, raw: f32, alpha: f32) -> f32 {
    alpha * raw + (1.0 - alpha) * prev_smoothed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_one_passthrough() {
        assert_eq!(smooth(70.0, 45.0, 1.0), 45.0);
    }

    #[test]
    fn alpha_quarter() {
        let result = smooth(70.0, 50.0, 0.25);
        let expected = 0.25 * 50.0 + 0.75 * 70.0;
        assert!((result - expected).abs() < 0.001);
    }

    #[test]
    fn convergence() {
        let mut s = 70.0;
        for _ in 0..100 {
            s = smooth(s, 50.0, 0.25);
        }
        assert!((s - 50.0).abs() < 0.01);
    }

    #[test]
    fn reinit_on_resume() {
        let raw = 40.0;
        let result = smooth(raw, raw, 0.25);
        assert_eq!(result, 40.0);
    }
}
