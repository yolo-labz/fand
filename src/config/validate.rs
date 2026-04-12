//! Config validation — FR-010..015, FR-064..068.
//!
//! Called after TOML deserialization to check semantic constraints
//! that serde cannot express (range limits, monotonicity, cross-field
//! relationships).

use super::schema::{Config, FanBinding, ValidationError};

/// Validate a parsed Config. Returns a list of all violations found
/// (not just the first one — report everything so the operator can
/// fix all issues in one edit).
#[allow(clippy::missing_errors_doc)]
pub fn validate(config: &Config) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // FR-012: poll_interval_ms in [100, 5000].
    if config.poll_interval_ms < 100 || config.poll_interval_ms > 5000 {
        errors.push(ValidationError::MissingRequired {
            field: format!(
                "poll_interval_ms must be in [100, 5000], got {}",
                config.poll_interval_ms
            ),
            fan_index: None,
        });
    }

    // FR-003: log_level must be a known value.
    match config.log_level.as_str() {
        "error" | "warn" | "info" | "debug" => {}
        other => {
            errors.push(ValidationError::MissingRequired {
                field: format!(
                    "log_level must be error|warn|info|debug, got '{other}'"
                ),
                fan_index: None,
            });
        }
    }

    // FR-004: at least one fan section.
    if config.fan.is_empty() {
        errors.push(ValidationError::MissingRequired {
            field: "fan: at least one [[fan]] section required".into(),
            fan_index: None,
        });
        return errors; // Can't validate further without fans.
    }

    // FR-014: no duplicate fan indices.
    let mut seen_indices = std::collections::HashSet::new();
    for fan in &config.fan {
        if !seen_indices.insert(fan.index) {
            errors.push(ValidationError::DuplicateFanIndex { index: fan.index });
        }
    }

    // Per-fan validation.
    for fan in &config.fan {
        validate_fan(fan, &mut errors);
    }

    errors
}

fn validate_fan(fan: &FanBinding, errors: &mut Vec<ValidationError>) {
    let idx = fan.index;

    // FR-006 / FR-067: sensors must be non-empty, each exactly 4 ASCII bytes.
    if fan.sensors.is_empty() {
        errors.push(ValidationError::EmptySensors { fan_index: idx });
    }
    for sensor in &fan.sensors {
        let name = match sensor {
            super::schema::SensorRef::Name(s) => s.as_str(),
            super::schema::SensorRef::Smc { smc } => smc.as_str(),
        };
        if name.len() != 4 || !name.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
            errors.push(ValidationError::UnknownSensor {
                sensor: name.to_string(),
                available: vec!["must be exactly 4 printable ASCII bytes".into()],
            });
        }
    }

    // FR-010: curve must have at least 2 breakpoints.
    if fan.curve.len() < 2 {
        errors.push(ValidationError::CurveTooShort {
            fan_index: idx,
            count: fan.curve.len(),
        });
        return; // Can't check monotonicity without enough points.
    }

    // FR-011: temperatures must be strictly monotonically increasing.
    // FR-068: reject NaN/infinity in curve points.
    for (i, &(temp, rpm)) in fan.curve.iter().enumerate() {
        if !temp.is_finite() || temp < 0.0 {
            errors.push(ValidationError::NonMonotoneTemp {
                fan_index: idx,
                bp: i,
                prev_temp: 0.0,
                temp,
            });
        }
        if !((rpm as f32).is_finite()) {
            errors.push(ValidationError::MissingRequired {
                field: format!("fan[{idx}] curve[{i}] RPM is not finite"),
                fan_index: Some(idx),
            });
        }
        if i > 0 {
            let prev_temp = fan.curve[i - 1].0;
            if temp <= prev_temp {
                errors.push(ValidationError::NonMonotoneTemp {
                    fan_index: idx,
                    bp: i,
                    prev_temp,
                    temp,
                });
            }
        }
    }

    // FR-008: hysteresis in [0.0, 10.0] (using down margin as the primary).
    if fan.hysteresis_down < 0.0 || fan.hysteresis_down > 10.0 {
        errors.push(ValidationError::HysteresisInverted {
            fan_index: idx,
            up: fan.hysteresis_up,
            down: fan.hysteresis_down,
        });
    }
}

/// First-pass validation: checks that can run without SMC access.
/// Returns Ok(()) if all checks pass, Err(vec) with all violations otherwise.
#[allow(clippy::missing_errors_doc)]
pub fn validate_or_exit(config: &Config) -> Result<(), Vec<ValidationError>> {
    let errors = validate(config);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::*;

    fn minimal_config() -> Config {
        Config {
            config_version: 1,
            poll_interval_ms: 500,
            log_level: "info".into(),
            control_socket_path: "/var/run/fand.sock".into(),
            control_socket_mode: 0o600,
            low_power_attenuation_default: 1.0,
            fan: vec![FanBinding {
                index: 0,
                sensors: vec![SensorRef::Name("Tf04".into())],
                fusion: "max".into(),
                curve: vec![(50.0, 2317), (80.0, 6550)],
                hysteresis_up: 1.0,
                hysteresis_down: 2.0,
                smoothing_alpha: 0.25,
                ramp_down_rpm_per_s: 600,
                panic_temp_c: 95.0,
                panic_hold_s: 10,
                min_start_rpm: None,
                low_power_attenuation: None,
                ac: None,
                battery: None,
            }],
        }
    }

    #[test]
    fn valid_config_passes() {
        assert!(validate(&minimal_config()).is_empty());
    }

    #[test]
    fn reject_empty_fans() {
        let mut c = minimal_config();
        c.fan.clear();
        let errs = validate(&c);
        assert!(!errs.is_empty());
    }

    #[test]
    fn reject_duplicate_fan_index() {
        let mut c = minimal_config();
        let mut f2 = c.fan[0].clone();
        f2.index = 0; // duplicate
        c.fan.push(f2);
        let errs = validate(&c);
        assert!(errs.iter().any(|e| matches!(e, ValidationError::DuplicateFanIndex { .. })));
    }

    #[test]
    fn reject_single_curve_point() {
        let mut c = minimal_config();
        c.fan[0].curve = vec![(50.0, 2317)];
        let errs = validate(&c);
        assert!(errs.iter().any(|e| matches!(e, ValidationError::CurveTooShort { .. })));
    }

    #[test]
    fn reject_non_monotone_temps() {
        let mut c = minimal_config();
        c.fan[0].curve = vec![(80.0, 6550), (50.0, 2317)];
        let errs = validate(&c);
        assert!(errs.iter().any(|e| matches!(e, ValidationError::NonMonotoneTemp { .. })));
    }

    #[test]
    fn reject_poll_interval_too_low() {
        let mut c = minimal_config();
        c.poll_interval_ms = 50;
        let errs = validate(&c);
        assert!(!errs.is_empty());
    }

    #[test]
    fn reject_poll_interval_too_high() {
        let mut c = minimal_config();
        c.poll_interval_ms = 10000;
        let errs = validate(&c);
        assert!(!errs.is_empty());
    }

    #[test]
    fn reject_bad_log_level() {
        let mut c = minimal_config();
        c.log_level = "verbose".into();
        let errs = validate(&c);
        assert!(!errs.is_empty());
    }

    #[test]
    fn reject_empty_sensors() {
        let mut c = minimal_config();
        c.fan[0].sensors.clear();
        let errs = validate(&c);
        assert!(errs.iter().any(|e| matches!(e, ValidationError::EmptySensors { .. })));
    }

    #[test]
    fn reject_bad_fourcc_length() {
        let mut c = minimal_config();
        c.fan[0].sensors = vec![SensorRef::Name("TooLong".into())];
        let errs = validate(&c);
        assert!(errs.iter().any(|e| matches!(e, ValidationError::UnknownSensor { .. })));
    }

    #[test]
    fn accept_boundary_poll_interval() {
        let mut c = minimal_config();
        c.poll_interval_ms = 100;
        assert!(validate(&c).is_empty());
        c.poll_interval_ms = 5000;
        assert!(validate(&c).is_empty());
    }
}
