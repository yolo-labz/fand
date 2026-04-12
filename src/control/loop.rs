use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::control::{curve, ema, fusion, hysteresis, panic as panic_mod, slew, state::ClampedRpm};

pub struct DaemonState {
    pub tick_count: u64,
    pub tick_latency_us: [u64; 6],
    pub total_panic_entries: u32,
    pub total_sensor_dropouts: u32,
    pub total_reclaim_events: u32,
    pub last_reload_at: Option<Instant>,
    pub last_tick_completed_at: Arc<AtomicU64>,
    pub shutdown: Arc<AtomicBool>,
    pub reload: Arc<AtomicBool>,
    pub sleep_pending: Arc<AtomicBool>,
    pub wake_pending: Arc<AtomicBool>,
    pub shutdown_count: Arc<AtomicU8>,
}

impl DaemonState {
    #[must_use]
    pub fn new(
        last_tick: Arc<AtomicU64>,
        shutdown: Arc<AtomicBool>,
        reload: Arc<AtomicBool>,
        sleep_pending: Arc<AtomicBool>,
        wake_pending: Arc<AtomicBool>,
        shutdown_count: Arc<AtomicU8>,
    ) -> Self {
        Self {
            tick_count: 0,
            tick_latency_us: [0; 6],
            total_panic_entries: 0,
            total_sensor_dropouts: 0,
            total_reclaim_events: 0,
            last_reload_at: None,
            last_tick_completed_at: last_tick,
            shutdown,
            reload,
            sleep_pending,
            wake_pending,
            shutdown_count,
        }
    }

    pub fn record_latency(&mut self, us: u64) {
        let bucket = match us {
            0..=99 => 0,
            100..=999 => 1,
            1_000..=9_999 => 2,
            10_000..=99_999 => 3,
            100_000..=499_999 => 4,
            _ => 5,
        };
        self.tick_latency_us[bucket] = self.tick_latency_us[bucket].saturating_add(1);
    }
}

pub struct FanControlState {
    pub fan_index: u8,
    pub active: bool,
    pub min_rpm: f32,
    pub max_rpm: f32,
    pub driver_temp_raw: f32,
    pub driver_temp_smoothed: f32,
    pub hysteresis: hysteresis::HysteresisState,
    pub slew_limited_rpm: f32,
    pub final_target: Option<ClampedRpm>,
    pub panic_state: panic_mod::PanicState,
    pub consecutive_write_errors: u16,
    pub last_known_good_temp: f32,
    pub last_known_good_age_ticks: u32,
}

impl FanControlState {
    #[must_use]
    pub fn new(
        fan_index: u8,
        min_rpm: f32,
        max_rpm: f32,
        initial_actual_rpm: f32,
        initial_temp: f32,
    ) -> Self {
        Self {
            fan_index,
            active: false,
            min_rpm,
            max_rpm,
            driver_temp_raw: initial_temp,
            driver_temp_smoothed: initial_temp,
            hysteresis: hysteresis::HysteresisState::new(initial_actual_rpm, initial_temp),
            slew_limited_rpm: initial_actual_rpm,
            final_target: None,
            panic_state: panic_mod::PanicState::new(),
            consecutive_write_errors: 0,
            last_known_good_temp: initial_temp,
            last_known_good_age_ticks: 0,
        }
    }

    pub fn reinit_bumpless(&mut self, actual_rpm: f32, raw_temp: f32) {
        self.slew_limited_rpm = actual_rpm;
        self.driver_temp_smoothed = raw_temp;
        self.hysteresis.reinit(actual_rpm, raw_temp);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        sensor_values: &[f32],
        sensor_dropouts: &[bool],
        fusion_mode: fusion::FusionMode,
        breakpoints: &[(f32, u32)],
        alpha: f32,
        hyst_up: f32,
        hyst_down: f32,
        ramp_down_rpm_per_s: f32,
        panic_temp_c: f32,
        panic_hold_s: u32,
        actual_dt_secs: f32,
        now: Instant,
    ) -> f32 {
        // Stage 2: Fusion
        self.driver_temp_raw = fusion::fuse(
            sensor_values,
            sensor_dropouts,
            fusion_mode,
            self.last_known_good_temp,
        );

        // Update last_known_good
        let any_valid = sensor_dropouts.iter().any(|&d| !d);
        if any_valid {
            self.last_known_good_temp = self.driver_temp_raw;
            self.last_known_good_age_ticks = 0;
        } else {
            self.last_known_good_age_ticks = self.last_known_good_age_ticks.saturating_add(1);
            if self.last_known_good_age_ticks > 60 {
                return self.max_rpm;
            }
        }

        // Stage 3: EMA
        self.driver_temp_smoothed =
            ema::smooth(self.driver_temp_smoothed, self.driver_temp_raw, alpha);

        // Stage 4: Curve eval
        let curve_rpm = curve::evaluate(breakpoints, self.driver_temp_smoothed);

        // Stage 5: Hysteresis
        let hyst_rpm = hysteresis::apply(
            &mut self.hysteresis,
            curve_rpm,
            self.driver_temp_smoothed,
            hyst_up,
            hyst_down,
        );

        // Stage 6: Slew
        self.slew_limited_rpm = slew::limit(
            self.slew_limited_rpm,
            hyst_rpm,
            ramp_down_rpm_per_s,
            actual_dt_secs,
        );

        // Stage 7: Panic (uses raw, not smoothed)
        let panic_action = panic_mod::check(
            &mut self.panic_state,
            self.driver_temp_raw,
            panic_temp_c,
            panic_hold_s,
            now,
        );

        let rpm = match panic_action {
            panic_mod::PanicAction::ForceFxMx => self.max_rpm,
            panic_mod::PanicAction::Passthrough => self.slew_limited_rpm,
        };

        // Stage 9: Clamp
        let clamped = ClampedRpm::new(rpm, self.min_rpm, self.max_rpm);
        self.final_target = Some(clamped);

        #[allow(clippy::cast_precision_loss)]
        let result = clamped.value() as f32;
        result
    }
}
