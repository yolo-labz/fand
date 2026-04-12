use serde::Deserialize;

/// Top-level fand config, deserialized from /etc/fand.toml.
///
/// FR-001: parsed from TOML at the --config path.
/// FR-069: deny_unknown_fields catches typos / injection.
/// FR-070: config_version for forward compatibility.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// FR-070: schema version for forward compatibility. Must be 1 for this release.
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    /// FR-002: global poll interval in ms. Valid range: [100, 5000].
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u32,
    /// FR-003: log verbosity level.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_socket_path")]
    pub control_socket_path: String,
    #[serde(default = "default_socket_mode")]
    pub control_socket_mode: u16,
    #[serde(default = "default_lp_attenuation")]
    pub low_power_attenuation_default: f32,
    /// FR-004: per-fan curve definitions.
    pub fan: Vec<FanBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanBinding {
    pub index: u8,
    pub sensors: Vec<SensorRef>,
    #[serde(default = "default_fusion")]
    pub fusion: String,
    pub curve: Vec<Breakpoint>,
    #[serde(default = "default_hysteresis_up")]
    pub hysteresis_up: f32,
    #[serde(default = "default_hysteresis_down")]
    pub hysteresis_down: f32,
    #[serde(default = "default_smoothing_alpha")]
    pub smoothing_alpha: f32,
    #[serde(default = "default_ramp_down")]
    pub ramp_down_rpm_per_s: u32,
    #[serde(default = "default_panic_temp")]
    pub panic_temp_c: f32,
    #[serde(default = "default_panic_hold")]
    pub panic_hold_s: u32,
    pub min_start_rpm: Option<u32>,
    pub low_power_attenuation: Option<f32>,
    pub ac: Option<PowerSourceOverride>,
    pub battery: Option<PowerSourceOverride>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SensorRef {
    Name(String),
    Smc { smc: String },
}

pub type Breakpoint = (f32, u32);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerSourceOverride {
    pub curve: Option<Vec<Breakpoint>>,
    pub hysteresis_up: Option<f32>,
    pub hysteresis_down: Option<f32>,
    pub smoothing_alpha: Option<f32>,
    pub ramp_down_rpm_per_s: Option<u32>,
    pub panic_temp_c: Option<f32>,
    pub panic_hold_s: Option<u32>,
}

fn default_config_version() -> u32 {
    1
}
fn default_poll_interval() -> u32 {
    500
}
fn default_log_level() -> String {
    "info".into()
}
fn default_socket_path() -> String {
    "/var/run/fand.sock".into()
}
fn default_socket_mode() -> u16 {
    0o600
}
fn default_lp_attenuation() -> f32 {
    1.0
}
fn default_fusion() -> String {
    "max".into()
}
fn default_hysteresis_up() -> f32 {
    1.0
}
fn default_hysteresis_down() -> f32 {
    3.0
}
fn default_smoothing_alpha() -> f32 {
    0.25
}
fn default_ramp_down() -> u32 {
    600
}
fn default_panic_temp() -> f32 {
    95.0
}
fn default_panic_hold() -> u32 {
    10
}

#[derive(Debug)]
pub enum ValidationError {
    TomlSyntax {
        line: usize,
        col: usize,
        message: String,
    },
    MissingRequired {
        field: String,
        fan_index: Option<u8>,
    },
    DuplicateFanIndex {
        index: u8,
    },
    UnknownFanIndex {
        index: u8,
        available: Vec<u8>,
    },
    EmptySensors {
        fan_index: u8,
    },
    UnknownSensor {
        sensor: String,
        available: Vec<String>,
    },
    CurveTooShort {
        fan_index: u8,
        count: usize,
    },
    CurveTooLong {
        fan_index: u8,
        count: usize,
    },
    NonMonotoneTemp {
        fan_index: u8,
        bp: usize,
        prev_temp: f32,
        temp: f32,
    },
    NonMonotoneRpm {
        fan_index: u8,
        bp: usize,
        prev_rpm: u32,
        rpm: u32,
    },
    RpmOutOfRange {
        fan_index: u8,
        bp: usize,
        rpm: u32,
        max: f32,
    },
    HysteresisInverted {
        fan_index: u8,
        up: f32,
        down: f32,
    },
    SmoothingAlphaRange {
        fan_index: u8,
        alpha: f32,
    },
    PanicTempTooLow {
        fan_index: u8,
        panic: f32,
        last_curve_temp: f32,
    },
    LowPowerRange {
        value: f32,
    },
    InvalidSocketMode {
        mode: u16,
    },
    FileTooLarge {
        size_bytes: u64,
    },
    NestingTooDeep {
        depth: usize,
    },
    UnknownField {
        key: String,
        context: String,
    },
    UnsafePermissions {
        path: String,
        owner: u32,
        mode: u32,
    },
}

impl core::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TomlSyntax { line, col, message } => {
                write!(f, "line {line}:{col}: TOML syntax error: {message}")
            }
            Self::MissingRequired { field, fan_index } => match fan_index {
                Some(i) => write!(f, "fan[{i}]: missing required field '{field}'"),
                None => write!(f, "missing required field '{field}'"),
            },
            Self::DuplicateFanIndex { index } => {
                write!(f, "duplicate fan index {index}")
            }
            Self::UnknownFanIndex { index, available } => {
                write!(f, "fan index {index} not found; available: {available:?}")
            }
            Self::EmptySensors { fan_index } => {
                write!(f, "fan[{fan_index}]: sensors list is empty")
            }
            Self::UnknownSensor { sensor, available } => {
                write!(f, "sensor '{sensor}' not found; available: {available:?}")
            }
            Self::CurveTooShort { fan_index, count } => {
                write!(
                    f,
                    "fan[{fan_index}]: curve has {count} breakpoints (minimum 2)"
                )
            }
            Self::CurveTooLong { fan_index, count } => {
                write!(
                    f,
                    "fan[{fan_index}]: curve has {count} breakpoints (maximum 100)"
                )
            }
            Self::NonMonotoneTemp {
                fan_index,
                bp,
                prev_temp,
                temp,
            } => {
                write!(
                    f,
                    "fan[{fan_index}]: breakpoint {bp} temp ({temp}°C) ≤ previous ({prev_temp}°C)"
                )
            }
            Self::NonMonotoneRpm {
                fan_index,
                bp,
                prev_rpm,
                rpm,
            } => {
                write!(
                    f,
                    "fan[{fan_index}]: breakpoint {bp} RPM ({rpm}) < previous ({prev_rpm})"
                )
            }
            Self::RpmOutOfRange {
                fan_index,
                bp,
                rpm,
                max,
            } => {
                write!(
                    f,
                    "fan[{fan_index}]: breakpoint {bp} RPM ({rpm}) exceeds max ({max})"
                )
            }
            Self::HysteresisInverted {
                fan_index,
                up,
                down,
            } => {
                write!(
                    f,
                    "fan[{fan_index}]: hysteresis_down ({down}) must be ≥ hysteresis_up ({up})"
                )
            }
            Self::SmoothingAlphaRange { fan_index, alpha } => {
                write!(
                    f,
                    "fan[{fan_index}]: smoothing_alpha ({alpha}) must be in (0.0, 1.0]"
                )
            }
            Self::PanicTempTooLow {
                fan_index,
                panic,
                last_curve_temp,
            } => {
                write!(f, "fan[{fan_index}]: panic_temp_c ({panic}) must be > last curve temp ({last_curve_temp})")
            }
            Self::LowPowerRange { value } => {
                write!(f, "low_power_attenuation ({value}) must be in [0.3, 1.0]")
            }
            Self::InvalidSocketMode { mode } => {
                write!(f, "control_socket_mode ({mode:#o}) must be 0o600 or 0o660")
            }
            Self::FileTooLarge { size_bytes } => {
                write!(f, "config file too large ({size_bytes} bytes, max 65536)")
            }
            Self::NestingTooDeep { depth } => {
                write!(f, "TOML nesting too deep ({depth} levels, max 32)")
            }
            Self::UnknownField { key, context } => {
                write!(f, "unknown field '{key}' in {context}")
            }
            Self::UnsafePermissions { path, owner, mode } => {
                write!(
                    f,
                    "{path}: unsafe permissions (owner uid {owner}, mode {mode:#o})"
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}
