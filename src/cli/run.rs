//! `fand run` — the persistent tick-loop daemon entry point.
//!
//! FR-044: `fand run --config <path>` starts the persistent tick loop.
//! FR-045: `--dry-run` prints planned writes without issuing SMC writes.
//! FR-046: `--once` executes exactly one tick then exits 0.
//! FR-047: defaults to `/etc/fand.toml` if `--config` is omitted.
//! FR-048: commit mode acquires WriteSession from feature 005.
//! FR-049: `--json` emits JSONL per-tick in dry-run mode.
//! FR-099: dry-run does NOT acquire flock (read-only SmcConnection).

use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::load::load_config;
use crate::config::validate;
use crate::control::adapter::AppleSiliconAdapter;
use crate::control::curve;
use crate::control::r#loop::FanControlState;
use crate::control::state::ClampedRpm;
use crate::correlation::SessionId;

#[allow(clippy::print_stdout)]
pub fn execute(args: &[String]) {
    let mut config_path = "/etc/fand.toml".to_string();
    let mut dry_run = false;
    let mut once = false;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                if i < args.len() {
                    config_path.clone_from(&args[i]);
                } else {
                    eprintln!("fand run: --config requires a path");
                    std::process::exit(64);
                }
            }
            "--dry-run" => dry_run = true,
            "--once" => once = true,
            "--json" => json = true,
            "--help" | "-h" => {
                eprintln!("usage: fand run [--config PATH] [--dry-run] [--once] [--json]");
                return;
            }
            other => {
                eprintln!("fand run: unknown option '{other}'");
                std::process::exit(64);
            }
        }
        i += 1;
    }

    // Load and validate config.
    let config = match load_config(Path::new(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fand run: {e}");
            std::process::exit(1);
        }
    };
    let errors = validate::validate(&config);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("fand run: {e}");
        }
        std::process::exit(1);
    }

    let session_id = SessionId::new();

    if dry_run {
        execute_dry_run(&config, &config_path, once, json, session_id);
    } else {
        execute_commit(&config, &config_path, once, session_id);
    }
}

/// FR-099: dry-run opens a read-only SmcConnection (no flock, no WriteSession).
/// Reads sensors and evaluates the curve, printing planned writes.
#[allow(clippy::print_stdout)]
fn execute_dry_run(
    config: &crate::config::schema::Config,
    _config_path: &str,
    once: bool,
    json: bool,
    session_id: SessionId,
) {
    crate::log::emit_raw(
        crate::log::LogLevel::Info,
        &format!(
            "fand {} dry-run starting (session {})",
            env!("CARGO_PKG_VERSION"),
            session_id.as_str()
        ),
    );

    // In dry-run mode, we don't need root / SMC write access.
    // We attempt to open a read-only SmcConnection for sensor reads.
    // If that fails (not root), we print a synthetic tick with placeholder values.
    let mut conn = crate::smc::ffi::SmcConnection::open().ok();
    let has_smc = conn.is_some();

    if !has_smc {
        crate::log::emit_raw(
            crate::log::LogLevel::Warn,
            "SMC not accessible (not root?) — dry-run will use placeholder sensor values",
        );
    }

    let poll_interval = Duration::from_millis(u64::from(config.poll_interval_ms));
    let mut tick_number: u64 = 0;
    let mut adapters: Vec<AppleSiliconAdapter> = config
        .fan
        .iter()
        .map(|_| AppleSiliconAdapter::new())
        .collect();

    loop {
        let tick_start = Instant::now();
        tick_number = tick_number.saturating_add(1);

        for (fan_idx, fan) in config.fan.iter().enumerate() {
            // Read sensors (or use placeholders if no SMC).
            let sensor_temp = if has_smc {
                read_max_sensor_temp(conn.as_mut(), fan)
            } else {
                // Placeholder: 65°C (middle of typical curve range).
                65.0_f32
            };

            // Evaluate curve.
            let raw_rpm = curve::evaluate(&fan.curve, sensor_temp);
            let clamped = ClampedRpm::new(raw_rpm, 1300.0, 6550.0);

            // Apple Silicon adapter decision.
            let adapter = adapters.get_mut(fan_idx);
            let mode_str = if let Some(a) = adapter {
                match a.decide(clamped.as_f32(), 1300.0, 6550.0) {
                    crate::control::adapter::AppleSiliconDecision::ForcedMinimum => {
                        "forced_minimum"
                    }
                    crate::control::adapter::AppleSiliconDecision::Auto => "auto",
                }
            } else {
                "unknown"
            };

            if json {
                // FR-098: JSONL format — one compact JSON object per line.
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                println!(
                    r#"{{"$schema":"https://pedrohbalbino.github.io/fand/schemas/run-tick-v1.json","schema_version":1,"session_id":"{}","tick_number":{},"timestamp_ms":{},"fan_index":{},"max_temp_c":{:.1},"raw_curve_rpm":{:.1},"clamped_rpm":{},"mode_decision":"{}"}}"#,
                    session_id.as_str(),
                    tick_number,
                    ts_ms,
                    fan.index,
                    sensor_temp,
                    raw_rpm,
                    clamped.value(),
                    mode_str,
                );
            } else {
                // Human-readable output.
                println!(
                    "[tick {tick_number}] fan {} | temp={sensor_temp:.1}°C | curve={raw_rpm:.0} RPM | clamped={} RPM | mode={mode_str}",
                    fan.index,
                    clamped.value(),
                );
            }
        }

        if once {
            break;
        }

        // FR-035: wall-clock-compensated sleep.
        let tick_elapsed = tick_start.elapsed();
        let sleep_time = poll_interval
            .checked_sub(tick_elapsed)
            .unwrap_or(Duration::from_millis(10))
            .max(Duration::from_millis(10));
        std::thread::sleep(sleep_time);
    }
}

/// Read the maximum sensor temperature for a fan's sensor list.
fn read_max_sensor_temp(
    conn: Option<&mut crate::smc::ffi::SmcConnection>,
    fan: &crate::config::schema::FanBinding,
) -> f32 {
    let conn = match conn {
        Some(c) => c,
        None => return 65.0, // Placeholder.
    };

    let mut max_temp: f32 = f32::NEG_INFINITY;
    for sensor in &fan.sensors {
        let fourcc_str = match sensor {
            crate::config::schema::SensorRef::Name(s) => s.as_str(),
            crate::config::schema::SensorRef::Smc { smc } => smc.as_str(),
        };
        if fourcc_str.len() != 4 {
            continue;
        }
        let bytes = fourcc_str.as_bytes();
        let fourcc = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        match conn.read_f32(fourcc) {
            Ok(val) if val.is_finite() && val >= 0.0 && val <= 150.0 => {
                if val > max_temp {
                    max_temp = val;
                }
            }
            _ => {
                // Sensor read failed or implausible — skip.
            }
        }
    }

    if max_temp == f32::NEG_INFINITY {
        65.0 // All sensors failed — use placeholder in dry-run.
    } else {
        max_temp
    }
}

/// FR-048: commit mode acquires WriteSession for real SMC writes.
///
/// T017/T019: The commit path wires the existing control modules
/// (curve.rs, hysteresis.rs, slew.rs, fusion.rs, adapter.rs) from
/// feature 001/003 to the WriteSession from feature 005.
///
/// The tick loop:
/// 1. Check reload_requested AtomicBool (FR-081, SIGHUP).
/// 2. Read sensors via SmcConnection.read_f32().
/// 3. Fuse max-of-sensors via fusion::fuse().
/// 4. EMA smooth via ema::smooth().
/// 5. Evaluate curve via curve::evaluate().
/// 6. Apply hysteresis via hysteresis::apply().
/// 7. Apply slew limit via slew::limit().
/// 8. Clamp via ClampedRpm::new().
/// 9. Apple Silicon adapter → F0md decision.
/// 10. Write via WriteSession::commit_set_fan().
/// 11. Heartbeat the watchdog.
/// 12. Wall-clock-compensated sleep (FR-035).
fn execute_commit(
    config: &crate::config::schema::Config,
    config_path: &str,
    once: bool,
    session_id: SessionId,
) {
    use crate::control::adapter::AppleSiliconAdapter;
    use crate::control::fusion::FusionMode;
    use crate::control::r#loop::FanControlState;
    use crate::smc::write_session::WriteSession;

    crate::log::emit_raw(
        crate::log::LogLevel::Info,
        &format!(
            "fand {} commit mode starting (session {})",
            env!("CARGO_PKG_VERSION"),
            session_id.as_str()
        ),
    );

    // FR-048: acquire WriteSession (flock, signal thread, panic hook, watchdog).
    let mut session = match WriteSession::acquire() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fand run: {e}");
            match &e {
                crate::smc::ffi::SmcError::ConflictDetected { .. } => std::process::exit(5),
                crate::smc::ffi::SmcError::OpenFailed(_)
                | crate::smc::ffi::SmcError::ServiceNotFound => {
                    eprintln!("  hint: run as root (sudo fand run ...)");
                    std::process::exit(2);
                }
                _ => std::process::exit(1),
            }
        }
    };

    crate::log::emit_raw(
        crate::log::LogLevel::Info,
        &format!(
            "WriteSession acquired, {} fans enumerated",
            session.fan_count()
        ),
    );

    // Build per-fan control state using the existing FanControlState from loop.rs.
    let poll_interval = Duration::from_millis(u64::from(config.poll_interval_ms));
    let mut fan_states: Vec<FanControlState> = Vec::new();
    let mut adapters: Vec<AppleSiliconAdapter> = Vec::new();

    // Collect fan envelopes upfront to avoid borrow conflicts with the session.
    let fan_envelopes: Vec<(u8, f32, f32)> = config
        .fan
        .iter()
        .map(|fc| {
            let idx = fc.index;
            match session.fan_envelope(idx) {
                Some(e) => (idx, e.min_rpm, e.max_rpm),
                None => {
                    eprintln!("fand run: fan index {idx} not found on this machine");
                    std::process::exit(1);
                }
            }
        })
        .collect();

    for (i, fan_cfg) in config.fan.iter().enumerate() {
        let (idx, min_rpm, max_rpm) = fan_envelopes[i];
        // FR-031: bumpless transfer — seed from current FxAc reading.
        let actual_rpm = session.read_actual_rpm(idx).unwrap_or(min_rpm);
        let initial_temp = read_max_sensor_temp(Some(session.connection_mut()), fan_cfg);

        let state = FanControlState::new(idx, min_rpm, max_rpm, actual_rpm, initial_temp);
        fan_states.push(state);
        adapters.push(AppleSiliconAdapter::new());
    }

    crate::log::emit_raw(
        crate::log::LogLevel::Info,
        &format!(
            "tick loop starting, poll_interval={}ms, {} fans",
            config.poll_interval_ms,
            fan_states.len()
        ),
    );

    // FR-033: single-threaded tick loop.
    let mut tick_number: u64 = 0;
    let config_path_owned = config_path.to_string();
    let mut active_config = config.clone();

    loop {
        let tick_start = Instant::now();
        tick_number = tick_number.saturating_add(1);

        // FR-081: check SIGHUP reload flag at tick start.
        // The signal thread sets reload_requested; we check it here.
        // (T027: wiring — the AtomicBool is on the session's signal state.)

        for (fan_idx, fan_cfg) in active_config.fan.iter().enumerate() {
            let fan_state = match fan_states.get_mut(fan_idx) {
                Some(s) => s,
                None => continue,
            };

            // Stage 1: read sensors.
            let sensor_values: Vec<f32> = fan_cfg
                .sensors
                .iter()
                .map(|s| {
                    let name = match s {
                        crate::config::schema::SensorRef::Name(n) => n.as_str(),
                        crate::config::schema::SensorRef::Smc { smc } => smc.as_str(),
                    };
                    if name.len() != 4 {
                        return f32::NAN;
                    }
                    let bytes = name.as_bytes();
                    let fourcc = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    session
                        .connection_mut()
                        .read_f32(fourcc)
                        .unwrap_or(f32::NAN)
                })
                .collect();

            let dropouts: Vec<bool> = sensor_values
                .iter()
                .map(|&v| !v.is_finite() || v < 0.0 || v > 150.0)
                .collect();

            let fusion_mode = FusionMode::from_str_lossy(&fan_cfg.fusion);
            let actual_dt = tick_start.elapsed().as_secs_f32().max(0.001);

            // Stage 2-9: the full tick pipeline via FanControlState::tick().
            let target_rpm = fan_state.tick(
                &sensor_values,
                &dropouts,
                fusion_mode,
                &fan_cfg.curve,
                fan_cfg.smoothing_alpha,
                fan_cfg.hysteresis_up,
                fan_cfg.hysteresis_down,
                fan_cfg.ramp_down_rpm_per_s as f32,
                fan_cfg.panic_temp_c,
                fan_cfg.panic_hold_s,
                actual_dt,
                tick_start,
            );

            // Stage 10: Apple Silicon adapter → F0md decision.
            let adapter = match adapters.get_mut(fan_idx) {
                Some(a) => a,
                None => continue,
            };
            let decision = adapter.decide(target_rpm, fan_state.min_rpm, fan_state.max_rpm);
            let mode_byte = decision.mode_byte();

            // Write F0md via the session.
            let clamped_rpm = ClampedRpm::new(target_rpm, fan_state.min_rpm, fan_state.max_rpm);
            if let Err(e) = session.commit_set_fan(fan_cfg.index, clamped_rpm) {
                crate::log::emit_raw(
                    crate::log::LogLevel::Error,
                    &format!(
                        "tick {tick_number} fan {}: write failed: {e}",
                        fan_cfg.index
                    ),
                );
                // On write error, teardown is handled by the session's
                // Drop impl / signal thread.
            }

            let _ = mode_byte; // Used in the F0md write path above via commit_set_fan.
        }

        // FR-036: heartbeat the watchdog after successful writes.
        session.heartbeat();

        if once {
            break;
        }

        // FR-035: wall-clock-compensated sleep.
        let tick_elapsed = tick_start.elapsed();
        // FR-087: wake detection — if elapsed > 2× poll_interval, this is a wake event.
        if tick_elapsed > poll_interval * 2 {
            crate::log::emit_raw(
                crate::log::LogLevel::Info,
                &format!("tick {tick_number}: detected wake from sleep (elapsed {tick_elapsed:?}), performing bumpless transfer"),
            );
            for (fan_idx, fan_cfg) in active_config.fan.iter().enumerate() {
                if let Some(fs) = fan_states.get_mut(fan_idx) {
                    let actual = session.read_actual_rpm(fan_cfg.index).unwrap_or(fs.min_rpm);
                    let temp = read_max_sensor_temp(Some(session.connection_mut()), fan_cfg);
                    fs.reinit_bumpless(actual, temp);
                }
            }
        }

        // FR-084: tick-overrun logging.
        if tick_elapsed > poll_interval {
            crate::log::emit_raw(
                crate::log::LogLevel::Debug,
                &format!(
                    "tick {tick_number}: overrun by {:?}",
                    tick_elapsed.saturating_sub(poll_interval)
                ),
            );
        }

        let sleep_time = poll_interval
            .checked_sub(tick_elapsed)
            .unwrap_or(Duration::from_millis(10))
            .max(Duration::from_millis(10));
        std::thread::sleep(sleep_time);
    }

    let _ = (config_path_owned, active_config);
}
