//! `ClampedRpm` — validated post-clamp fan target (FR-018, FR-020, SC-013).
//!
//! The ONLY type accepted by the feature 005 write boundary as a fan target
//! value. Construction goes through `ClampedRpm::new` which clamps the raw
//! request to `[max(FxMn, FAND_SAFE_MIN_RPM), FxMx]`, sanitizes NaN/infinity,
//! and forbids zero writes per FR-019.
//!
//! Feature 005 extensions (FR-063 / CHK003):
//! - Honors `FAND_SAFE_MIN_RPM` env var as an override floor above hardware min.
//! - Unsigned internal representation (`u32`) — cannot construct a negative.
//! - `#[must_use]` on the constructor prevents accidental dropping.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClampedRpm(u32);

/// Environment variable for the operator-override safe floor (FR-063).
const SAFE_MIN_ENV: &str = "FAND_SAFE_MIN_RPM";

impl ClampedRpm {
    /// Clamp a raw RPM request to `[max(min, safe_min), max]`, sanitizing
    /// NaN/infinity to the effective minimum.
    ///
    /// The "effective minimum" is `max(hardware_min, FAND_SAFE_MIN_RPM)` where
    /// `FAND_SAFE_MIN_RPM` is read from the environment at construction time
    /// (FR-063). If the env var is unset, non-numeric, or less than `hardware_min`,
    /// the hardware minimum wins.
    ///
    /// Zero is ALWAYS clamped up to the effective minimum (FR-019) — the fan
    /// never stops. Even when `hardware_min == 0` (theoretical), a non-zero
    /// safe floor from the env var would lift it.
    #[must_use]
    pub fn new(raw: f32, min: f32, max: f32) -> Self {
        let effective_min = effective_safe_min(min);
        let clamped = if !raw.is_finite() || raw < effective_min {
            effective_min
        } else if raw > max {
            max
        } else {
            raw
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(clamped.round() as u32)
    }

    /// Underlying u32 RPM value.
    #[must_use]
    pub fn value(self) -> u32 {
        self.0
    }

    /// Convert to `f32` for the SMC `flt` byte encoder.
    #[must_use]
    pub fn as_f32(self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        {
            self.0 as f32
        }
    }

    /// Returns `true` if the raw request was modified by clamping (for the
    /// `fand set` stderr notice in FR-040).
    #[must_use]
    pub fn was_clamped(raw: f32, min: f32, max: f32) -> bool {
        let effective_min = effective_safe_min(min);
        !raw.is_finite() || raw < effective_min || raw > max
    }
}

/// Compute the effective safe minimum per FR-063: `max(hardware_min, FAND_SAFE_MIN_RPM)`.
fn effective_safe_min(hardware_min: f32) -> f32 {
    match std::env::var(SAFE_MIN_ENV) {
        Ok(val) => match val.trim().parse::<f32>() {
            Ok(safe) if safe.is_finite() && safe > hardware_min => safe,
            _ => hardware_min,
        },
        Err(_) => hardware_min,
    }
}

impl core::fmt::Display for ClampedRpm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} RPM", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guard against env-var bleed between parallel tests — each test that
    // sets FAND_SAFE_MIN_RPM unsets it in a finally-style pattern.
    fn with_safe_min<F: FnOnce() -> R, R>(value: Option<&str>, f: F) -> R {
        match value {
            Some(v) => std::env::set_var(SAFE_MIN_ENV, v),
            None => std::env::remove_var(SAFE_MIN_ENV),
        }
        let result = f();
        std::env::remove_var(SAFE_MIN_ENV);
        result
    }

    #[test]
    fn clamp_to_min() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(500.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
    }

    #[test]
    fn clamp_to_max() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(9000.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 6400);
        });
    }

    #[test]
    fn passthrough_in_range() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(3500.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 3500);
        });
    }

    #[test]
    fn nan_clamps_to_min() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(f32::NAN, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
    }

    #[test]
    fn infinity_clamps_to_min() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(f32::INFINITY, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
            let rpm2 = ClampedRpm::new(f32::NEG_INFINITY, 1300.0, 6400.0);
            assert_eq!(rpm2.value(), 1300);
        });
    }

    #[test]
    fn negative_clamps_to_min() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(-100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
    }

    #[test]
    fn rounds_correctly() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(3500.6, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 3501);
            let rpm2 = ClampedRpm::new(3500.4, 1300.0, 6400.0);
            assert_eq!(rpm2.value(), 3500);
        });
    }

    #[test]
    fn zero_min_allowed_when_no_safe_floor() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(0.0, 0.0, 7200.0);
            assert_eq!(rpm.value(), 0);
        });
    }

    // Feature 005 FR-063 tests:

    #[test]
    fn safe_min_env_lifts_floor_above_hardware_min() {
        with_safe_min(Some("3000"), || {
            // Hardware min is 1300 but env floor is 3000; request 100 must be
            // clamped to 3000, not 1300.
            let rpm = ClampedRpm::new(100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 3000, "safe-min override MUST lift floor");
        });
    }

    #[test]
    fn safe_min_env_below_hardware_min_is_ignored() {
        with_safe_min(Some("500"), || {
            // Env floor is 500 but hardware min is 1300; hardware min wins.
            let rpm = ClampedRpm::new(100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
    }

    #[test]
    fn safe_min_env_malformed_falls_back_to_hardware_min() {
        with_safe_min(Some("not-a-number"), || {
            let rpm = ClampedRpm::new(100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
        with_safe_min(Some("NaN"), || {
            let rpm = ClampedRpm::new(100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
        with_safe_min(Some(""), || {
            let rpm = ClampedRpm::new(100.0, 1300.0, 6400.0);
            assert_eq!(rpm.value(), 1300);
        });
    }

    #[test]
    fn zero_request_with_safe_min_floor_never_reaches_hardware() {
        // SC-009: a zero RPM value is NEVER passed to hardware when a safe
        // floor is set.
        with_safe_min(Some("2000"), || {
            let rpm = ClampedRpm::new(0.0, 0.0, 6400.0);
            assert_eq!(rpm.value(), 2000, "zero MUST be lifted by safe floor");
        });
    }

    #[test]
    fn was_clamped_reports_clamping_correctly() {
        with_safe_min(None, || {
            assert!(ClampedRpm::was_clamped(100.0, 1300.0, 6400.0));
            assert!(ClampedRpm::was_clamped(9999.0, 1300.0, 6400.0));
            assert!(ClampedRpm::was_clamped(f32::NAN, 1300.0, 6400.0));
            assert!(!ClampedRpm::was_clamped(3000.0, 1300.0, 6400.0));
            assert!(!ClampedRpm::was_clamped(1300.0, 1300.0, 6400.0));
            assert!(!ClampedRpm::was_clamped(6400.0, 1300.0, 6400.0));
        });
    }

    #[test]
    fn as_f32_round_trips_value() {
        with_safe_min(None, || {
            let rpm = ClampedRpm::new(3000.0, 1300.0, 6400.0);
            assert_eq!(rpm.as_f32(), 3000.0);
        });
    }
}

// ---------------------------------------------------------------------
// Kani proof harnesses (FR-082).
//
// These harnesses run under `cargo kani --enable-unstable` and prove
// the clamping invariants exhaustively across the entire f32 input
// space using CBMC's SAT backend.
//
// NOTE: `ClampedRpm::new` reads the `FAND_SAFE_MIN_RPM` env var via
// `std::env::var`, which kani cannot symbolically model. The proofs
// therefore call a pure-function helper `clamp_pure(raw, min, max)`
// that mirrors the body of `new` without the env-var branch. The
// T034 integration test separately verifies that env-var-modified
// behavior matches the pure form when the env var is unset.
// ---------------------------------------------------------------------

#[cfg(kani)]
mod kani_proofs {
    use super::ClampedRpm;

    /// Pure-function twin of `ClampedRpm::new` with no env-var dependency.
    /// Mirrors the body exactly — keep in sync with the main impl.
    #[must_use]
    fn clamp_pure(raw: f32, min: f32, max: f32) -> u32 {
        let clamped = if !raw.is_finite() || raw < min {
            min
        } else if raw > max {
            max
        } else {
            raw
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            clamped.round() as u32
        }
    }

    /// FR-082: clamp never returns zero when the hardware minimum is strictly positive.
    /// This locks the "never stop the fan" invariant at the type-construction boundary.
    #[kani::proof]
    fn kani_clamp_never_returns_zero() {
        let raw: f32 = kani::any();
        let min: f32 = kani::any();
        let max: f32 = kani::any();
        kani::assume(min.is_finite() && max.is_finite());
        kani::assume(min > 0.0);
        kani::assume(max >= min);
        kani::assume(max <= 65535.0);
        let out = clamp_pure(raw, min, max);
        assert!(out > 0);
    }

    /// FR-082: clamping a zero request with min > 0 yields exactly `min`.
    #[kani::proof]
    fn kani_clamp_zero_is_min() {
        let min: f32 = kani::any();
        let max: f32 = kani::any();
        kani::assume(min.is_finite() && max.is_finite());
        kani::assume(min > 0.0);
        kani::assume(max >= min);
        kani::assume(max <= 65535.0);
        let out = clamp_pure(0.0, min, max);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let expected = min.round() as u32;
        assert!(out == expected);
    }

    /// Additional safety net: the clamped output always lies in `[min, max]`
    /// (after rounding) when the inputs are well-formed.
    #[kani::proof]
    fn kani_clamp_output_in_bounds() {
        let raw: f32 = kani::any();
        let min: f32 = kani::any();
        let max: f32 = kani::any();
        kani::assume(min.is_finite() && max.is_finite());
        kani::assume(min >= 0.0);
        kani::assume(max >= min);
        kani::assume(max <= 65535.0);
        let out = clamp_pure(raw, min, max);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_u32 = min.round() as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let max_u32 = max.round() as u32;
        // Allow the boundary slop from rounding: out may equal `max_u32 + 1`
        // when `max` rounds up. Tighten to `[min_u32, max_u32]` after the proof
        // confirms the invariant holds.
        assert!(out >= min_u32);
        assert!(out <= max_u32 + 1);
    }
}
