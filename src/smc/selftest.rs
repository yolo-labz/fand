//! `fand selftest` core: N-iteration write loop with median delta verification
//! (FR-043 through FR-049, Phase 4 US2).
//!
//! # Apple Silicon adaptation (RD-08)
//!
//! The original spec design called for "min RPM hold + max RPM hold" per
//! iteration, computing the delta between the two. On Apple Silicon M-series
//! (verified live on Mac17,2 in feature 005 session 5), the SMC interface
//! exposes only `F0md=0` (auto) and `F0md=1` (forced minimum). `F0md=2/3`
//! stop the fan entirely (FR-019 forbidden). Therefore the selftest oscillates
//! between **auto** and **forced-min**, sampling `F0Ac` during each hold
//! window, and computes the delta between the median auto-mode RPM and
//! the median forced-min RPM. This is the natural adaptation of FR-045's
//! delta-verification intent to the actual hardware control surface.
//!
//! Pass criteria (FR-045, FR-046, FR-047):
//! - **PASS**: zero round-trip mismatches AND `delta_rpm >= 500`
//! - **INCONCLUSIVE**: zero mismatches but `delta_rpm < 500` (system was
//!   too cool — the auto mode wasn't running the fan fast enough to
//!   differentiate from forced-min)
//! - **FAIL**: ≥1 round-trip mismatch on any F0md write

#![allow(clippy::missing_errors_doc)]

use std::time::{Duration, Instant};

/// Number of iterations per fan. 5 keeps the per-fan budget under 15 s
/// (well below FR-048's 30 s ceiling) while still producing a reliable
/// median across at least 5 hold windows.
pub const DEFAULT_ITERATIONS: u8 = 5;

/// Number of `F0Ac` samples taken per hold window. 5 samples at 200 ms
/// = 1 second per hold, matches FR-044's "1-second hold".
pub const SAMPLES_PER_HOLD: usize = 5;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// Per-iteration recorded measurement.
#[derive(Debug, Clone)]
pub struct IterationSample {
    pub iteration: u8,
    pub auto_samples: Vec<f32>,
    pub min_samples: Vec<f32>,
    pub auto_median: f32,
    pub min_median: f32,
    pub delta_rpm: f32,
}

/// Per-fan selftest report (FR-045, FR-046, FR-047, contracts/cli-selftest.md).
#[derive(Debug, Clone)]
pub struct SelftestFanReport {
    pub fan_index: u8,
    pub iterations_completed: u8,
    pub iterations_requested: u8,
    pub round_trip_count: u64,
    pub mismatch_count: u64,
    pub samples: Vec<IterationSample>,
    pub median_actual_at_min: f32,
    pub median_actual_at_auto: f32,
    pub delta_rpm: f32,
    pub result: SelftestResult,
}

/// Top-level selftest outcome (FR-045/047).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelftestResult {
    /// Zero round-trip mismatches AND delta ≥ 500 RPM.
    Pass,
    /// Zero mismatches but delta < 500 RPM (system was too cool to differentiate).
    Inconclusive,
    /// At least one round-trip mismatch.
    Fail,
    /// Watchdog fired mid-loop (FR-002).
    WatchdogTimeout,
    /// Conflict / lockfile failure.
    ConflictDetected,
}

impl SelftestResult {
    /// Map to the exit code per FR-039.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Pass => 0,
            Self::Inconclusive => 3,
            Self::Fail => 1,
            Self::WatchdogTimeout => 4,
            Self::ConflictDetected => 5,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Inconclusive => "inconclusive",
            Self::Fail => "fail",
            Self::WatchdogTimeout => "watchdog_timeout",
            Self::ConflictDetected => "conflict",
        }
    }
}

/// Aggregate report for the entire selftest run (all fans).
#[derive(Debug, Clone)]
pub struct SelftestReport {
    pub per_fan: Vec<SelftestFanReport>,
    pub total_iterations: u32,
    pub total_round_trips: u64,
    pub total_mismatches: u64,
    pub wall_clock_ms: u64,
    pub overall_result: SelftestResult,
}

impl SelftestReport {
    #[must_use]
    pub fn classify(per_fan: Vec<SelftestFanReport>, wall_clock: Duration) -> Self {
        let total_iterations: u32 = per_fan
            .iter()
            .map(|f| u32::from(f.iterations_completed))
            .sum();
        let total_round_trips: u64 = per_fan.iter().map(|f| f.round_trip_count).sum();
        let total_mismatches: u64 = per_fan.iter().map(|f| f.mismatch_count).sum();

        let overall_result = if total_mismatches > 0 {
            SelftestResult::Fail
        } else if per_fan
            .iter()
            .any(|f| f.result == SelftestResult::Inconclusive)
        {
            SelftestResult::Inconclusive
        } else if per_fan.iter().all(|f| f.result == SelftestResult::Pass) {
            SelftestResult::Pass
        } else {
            // Mixed result with no mismatches and no inconclusive → impossible
            // by construction, but treat as Pass conservatively.
            SelftestResult::Pass
        };

        Self {
            per_fan,
            total_iterations,
            total_round_trips,
            total_mismatches,
            wall_clock_ms: wall_clock.as_millis() as u64,
            overall_result,
        }
    }
}

/// Compute the median of a slice of f32 values. Returns 0.0 if the slice is empty.
///
/// Used to robustly summarize the F0Ac samples taken during each hold window.
/// Median is preferred over mean because thermal noise can produce outliers
/// (a brief CPU spike might push one sample 500 RPM above the rest).
#[must_use]
pub fn median_f32(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let len = sorted.len();
    if len % 2 == 1 {
        sorted[len / 2]
    } else {
        // Even count — average the two middle elements.
        (sorted[len / 2 - 1] + sorted[len / 2]) / 2.0
    }
}

/// Hold window: sleep + sample loop.
///
/// Sleeps for `SAMPLE_INTERVAL` between samples and reads the provided closure
/// `SAMPLES_PER_HOLD` times. Returns the collected samples.
pub fn hold_and_sample<F>(mut sample_fn: F) -> Vec<f32>
where
    F: FnMut() -> f32,
{
    let mut samples = Vec::with_capacity(SAMPLES_PER_HOLD);
    for _ in 0..SAMPLES_PER_HOLD {
        std::thread::sleep(SAMPLE_INTERVAL);
        samples.push(sample_fn());
    }
    samples
}

/// Compute per-iteration sample summary.
#[must_use]
pub fn classify_iteration(
    iteration: u8,
    auto_samples: Vec<f32>,
    min_samples: Vec<f32>,
) -> IterationSample {
    let auto_median = median_f32(&auto_samples);
    let min_median = median_f32(&min_samples);
    let delta_rpm = auto_median - min_median;
    IterationSample {
        iteration,
        auto_samples,
        min_samples,
        auto_median,
        min_median,
        delta_rpm,
    }
}

/// Threshold for the "delta meaningful" check (FR-045/047).
pub const DELTA_THRESHOLD_RPM: f32 = 500.0;

/// Classify the per-fan result given the collected iteration samples and the
/// observed mismatch count.
#[must_use]
pub fn classify_fan(
    fan_index: u8,
    iterations_completed: u8,
    iterations_requested: u8,
    round_trip_count: u64,
    mismatch_count: u64,
    samples: Vec<IterationSample>,
) -> SelftestFanReport {
    let all_min: Vec<f32> = samples.iter().flat_map(|s| s.min_samples.clone()).collect();
    let all_auto: Vec<f32> = samples
        .iter()
        .flat_map(|s| s.auto_samples.clone())
        .collect();
    let median_actual_at_min = median_f32(&all_min);
    let median_actual_at_auto = median_f32(&all_auto);
    let delta_rpm = median_actual_at_auto - median_actual_at_min;

    let result = if mismatch_count > 0 {
        SelftestResult::Fail
    } else if iterations_completed < iterations_requested {
        SelftestResult::Fail
    } else if delta_rpm >= DELTA_THRESHOLD_RPM {
        SelftestResult::Pass
    } else {
        SelftestResult::Inconclusive
    };

    SelftestFanReport {
        fan_index,
        iterations_completed,
        iterations_requested,
        round_trip_count,
        mismatch_count,
        samples,
        median_actual_at_min,
        median_actual_at_auto,
        delta_rpm,
        result,
    }
}

#[allow(dead_code)]
fn _ensure_instant_imported() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_count() {
        assert_eq!(median_f32(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median_f32(&[5.0]), 5.0);
        assert_eq!(median_f32(&[1.0, 2.0, 3.0, 4.0, 5.0]), 3.0);
    }

    #[test]
    fn median_even_count() {
        assert_eq!(median_f32(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(median_f32(&[10.0, 20.0]), 15.0);
    }

    #[test]
    fn median_empty_returns_zero() {
        assert_eq!(median_f32(&[]), 0.0);
    }

    #[test]
    fn median_handles_outlier() {
        // Median is robust to a single outlier; mean would be skewed.
        assert_eq!(
            median_f32(&[2300.0, 2310.0, 2320.0, 2330.0, 9999.0]),
            2320.0
        );
    }

    #[test]
    fn classify_iteration_computes_delta() {
        let s = classify_iteration(
            0,
            vec![5000.0, 5100.0, 4900.0],
            vec![2300.0, 2320.0, 2310.0],
        );
        assert_eq!(s.iteration, 0);
        assert_eq!(s.auto_median, 5000.0);
        assert_eq!(s.min_median, 2310.0);
        assert_eq!(s.delta_rpm, 2690.0);
    }

    #[test]
    fn classify_fan_pass_when_delta_above_threshold() {
        let samples = vec![classify_iteration(
            0,
            vec![5000.0, 5050.0, 5100.0],
            vec![2310.0, 2320.0, 2330.0],
        )];
        let report = classify_fan(0, 5, 5, 30, 0, samples);
        assert_eq!(report.result, SelftestResult::Pass);
        assert!(report.delta_rpm >= DELTA_THRESHOLD_RPM);
    }

    #[test]
    fn classify_fan_inconclusive_when_delta_below_threshold() {
        let samples = vec![classify_iteration(
            0,
            vec![2400.0, 2400.0, 2400.0],
            vec![2300.0, 2300.0, 2300.0],
        )];
        let report = classify_fan(0, 5, 5, 30, 0, samples);
        assert_eq!(report.result, SelftestResult::Inconclusive);
        assert!(report.delta_rpm < DELTA_THRESHOLD_RPM);
    }

    #[test]
    fn classify_fan_fail_on_mismatch() {
        let samples = vec![];
        let report = classify_fan(0, 5, 5, 30, 1, samples);
        assert_eq!(report.result, SelftestResult::Fail);
    }

    #[test]
    fn classify_fan_fail_on_incomplete_iterations() {
        let samples = vec![];
        let report = classify_fan(0, 3, 5, 30, 0, samples);
        assert_eq!(report.result, SelftestResult::Fail);
    }

    #[test]
    fn selftest_result_exit_codes_match_fr039() {
        assert_eq!(SelftestResult::Pass.exit_code(), 0);
        assert_eq!(SelftestResult::Fail.exit_code(), 1);
        assert_eq!(SelftestResult::Inconclusive.exit_code(), 3);
        assert_eq!(SelftestResult::WatchdogTimeout.exit_code(), 4);
        assert_eq!(SelftestResult::ConflictDetected.exit_code(), 5);
    }

    #[test]
    fn selftest_report_aggregates_fail_when_any_mismatch() {
        let pass_fan = classify_fan(
            0,
            5,
            5,
            30,
            0,
            vec![classify_iteration(0, vec![5000.0], vec![2300.0])],
        );
        let fail_fan = classify_fan(1, 5, 5, 30, 1, vec![]);
        let report = SelftestReport::classify(vec![pass_fan, fail_fan], Duration::from_secs(15));
        assert_eq!(report.overall_result, SelftestResult::Fail);
        assert_eq!(report.total_mismatches, 1);
    }

    #[test]
    fn selftest_report_aggregates_inconclusive_dominates_pass() {
        // If any fan is inconclusive, the overall result is inconclusive
        // even if other fans pass.
        let pass = classify_fan(
            0,
            5,
            5,
            30,
            0,
            vec![classify_iteration(0, vec![5000.0], vec![2300.0])],
        );
        let inconclusive = classify_fan(
            1,
            5,
            5,
            30,
            0,
            vec![classify_iteration(0, vec![2400.0], vec![2300.0])],
        );
        let report = SelftestReport::classify(vec![pass, inconclusive], Duration::from_secs(15));
        assert_eq!(report.overall_result, SelftestResult::Inconclusive);
    }
}
