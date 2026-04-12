//! Apple Silicon write-boundary adapter (FR-037, FR-089, FR-090, FR-091).
//!
//! On M-series SoCs, the SMC only accepts F0md=0 (auto) and F0md=1
//! (forced minimum) — arbitrary RPM targets are read-only (RD-08 from
//! feature 005). This adapter maps the curve evaluator's continuous RPM
//! output to a binary F0md decision at the write boundary.
//!
//! Engage threshold: target ≤ min + 5% of (max - min)  → F0md=1
//! Disengage threshold: target > min + 10% of (max - min) → F0md=0
//! The 5%/10% asymmetry prevents mode oscillation near the threshold.

/// The binary decision for Apple Silicon M-series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleSiliconDecision {
    /// F0md=1 — forced minimum RPM. Fan runs at hardware floor.
    ForcedMinimum,
    /// F0md=0 — auto mode. thermalmonitord controls the fan.
    Auto,
}

impl AppleSiliconDecision {
    /// The F0md value to write to the SMC.
    #[must_use]
    pub fn mode_byte(self) -> u8 {
        match self {
            Self::ForcedMinimum => 1,
            Self::Auto => 0,
        }
    }
}

/// Stateful adapter with engage/disengage hysteresis to prevent toggling.
#[derive(Debug)]
pub struct AppleSiliconAdapter {
    /// Current mode — starts as Auto until the first evaluate().
    current: AppleSiliconDecision,
}

impl AppleSiliconAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: AppleSiliconDecision::Auto,
        }
    }

    /// Evaluate the curve output and return the F0md decision.
    ///
    /// FR-089: engage threshold = min + 5% of (max - min).
    /// FR-090: disengage threshold = min + 10% of (max - min).
    /// The asymmetry creates a deadband that prevents rapid toggling.
    #[must_use]
    pub fn decide(&mut self, target_rpm: f32, min_rpm: f32, max_rpm: f32) -> AppleSiliconDecision {
        let range = max_rpm - min_rpm;
        let engage_threshold = min_rpm + range * 0.05;
        let disengage_threshold = min_rpm + range * 0.10;

        self.current = match self.current {
            AppleSiliconDecision::Auto => {
                if target_rpm <= engage_threshold {
                    AppleSiliconDecision::ForcedMinimum
                } else {
                    AppleSiliconDecision::Auto
                }
            }
            AppleSiliconDecision::ForcedMinimum => {
                if target_rpm > disengage_threshold {
                    AppleSiliconDecision::Auto
                } else {
                    AppleSiliconDecision::ForcedMinimum
                }
            }
        };
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f32 = 2317.0;
    const MAX: f32 = 6550.0;
    // 5% threshold = 2317 + 211.65 = 2528.65
    // 10% threshold = 2317 + 423.3 = 2740.3

    #[test]
    fn starts_as_auto() {
        let adapter = AppleSiliconAdapter::new();
        assert_eq!(adapter.current, AppleSiliconDecision::Auto);
    }

    #[test]
    fn engage_at_min_rpm() {
        let mut a = AppleSiliconAdapter::new();
        assert_eq!(a.decide(MIN, MIN, MAX), AppleSiliconDecision::ForcedMinimum);
    }

    #[test]
    fn stay_auto_above_engage_threshold() {
        let mut a = AppleSiliconAdapter::new();
        assert_eq!(a.decide(3000.0, MIN, MAX), AppleSiliconDecision::Auto);
    }

    #[test]
    fn hysteresis_prevents_rapid_toggle() {
        let mut a = AppleSiliconAdapter::new();
        // Engage
        assert_eq!(
            a.decide(2400.0, MIN, MAX),
            AppleSiliconDecision::ForcedMinimum
        );
        // Between 5% and 10% — stays forced minimum due to disengage hysteresis
        assert_eq!(
            a.decide(2600.0, MIN, MAX),
            AppleSiliconDecision::ForcedMinimum
        );
        // Above 10% — disengages
        assert_eq!(a.decide(2800.0, MIN, MAX), AppleSiliconDecision::Auto);
        // Back between 5% and 10% — stays auto due to engage hysteresis
        assert_eq!(a.decide(2600.0, MIN, MAX), AppleSiliconDecision::Auto);
        // Below 5% — engages again
        assert_eq!(
            a.decide(2400.0, MIN, MAX),
            AppleSiliconDecision::ForcedMinimum
        );
    }

    #[test]
    fn mode_byte_values() {
        assert_eq!(AppleSiliconDecision::ForcedMinimum.mode_byte(), 1);
        assert_eq!(AppleSiliconDecision::Auto.mode_byte(), 0);
    }
}
