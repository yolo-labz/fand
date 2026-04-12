#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionMode {
    Max,
    Mean,
}

impl FusionMode {
    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "mean" => Self::Mean,
            _ => Self::Max,
        }
    }
}

pub fn fuse(values: &[f32], dropouts: &[bool], mode: FusionMode, last_known_good: f32) -> f32 {
    let valid: Vec<f32> = values
        .iter()
        .zip(dropouts.iter())
        .filter(|(_, &d)| !d)
        .map(|(&v, _)| v)
        .collect();

    if valid.is_empty() {
        return last_known_good;
    }

    let result = match mode {
        FusionMode::Max => valid.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        FusionMode::Mean => {
            let sum: f32 = valid.iter().sum();
            #[allow(clippy::cast_precision_loss)]
            let count = valid.len() as f32;
            sum / count
        }
    };

    if result.is_nan() || result.is_infinite() {
        return last_known_good;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_of_three() {
        let vals = [60.0, 72.5, 65.0];
        let drops = [false, false, false];
        assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 72.5);
    }

    #[test]
    fn mean_of_three() {
        let vals = [60.0, 72.0, 66.0];
        let drops = [false, false, false];
        let result = fuse(&vals, &drops, FusionMode::Mean, 50.0);
        assert!((result - 66.0).abs() < 0.01);
    }

    #[test]
    fn excludes_dropout() {
        let vals = [60.0, 72.5, 65.0];
        let drops = [false, true, false];
        assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 65.0);
    }

    #[test]
    fn all_dropout_uses_fallback() {
        let vals = [60.0, 72.5];
        let drops = [true, true];
        assert_eq!(fuse(&vals, &drops, FusionMode::Max, 42.0), 42.0);
    }

    #[test]
    fn single_sensor() {
        let vals = [71.3];
        let drops = [false];
        assert_eq!(fuse(&vals, &drops, FusionMode::Max, 50.0), 71.3);
    }

    #[test]
    fn nan_guard() {
        let vals: [f32; 0] = [];
        let drops: [bool; 0] = [];
        assert_eq!(fuse(&vals, &drops, FusionMode::Mean, 55.0), 55.0);
    }
}
