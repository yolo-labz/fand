use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl LogLevel {
    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "error" => Self::Error,
            "warn" => Self::Warn,
            "info" => Self::Info,
            "debug" => Self::Debug,
            "trace" => Self::Trace,
            _ => Self::Info,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

static CURRENT_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

pub fn set_level(level: LogLevel) {
    CURRENT_LEVEL.store(level as u8, Ordering::Relaxed);
}

#[must_use]
pub fn current_level() -> LogLevel {
    match CURRENT_LEVEL.load(Ordering::Relaxed) {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        3 => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

#[inline]
#[must_use]
pub fn is_enabled(level: LogLevel) -> bool {
    (level as u8) <= CURRENT_LEVEL.load(Ordering::Relaxed)
}

static RATE_BUCKETS: [AtomicU32; 64] = {
    const INIT: AtomicU32 = AtomicU32::new(3);
    [INIT; 64]
};
static SUPPRESSED: [AtomicU32; 64] = {
    const INIT: AtomicU32 = AtomicU32::new(0);
    [INIT; 64]
};

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn elapsed_secs() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub fn rate_limited_emit(site_hash: usize, level: LogLevel, msg: &str) {
    let idx = site_hash % 64;
    let bucket = &RATE_BUCKETS[idx];
    let remaining = bucket.load(Ordering::Relaxed);
    if remaining == 0 {
        SUPPRESSED[idx].fetch_add(1, Ordering::Relaxed);
        return;
    }
    bucket.fetch_sub(1, Ordering::Relaxed);

    let suppressed = SUPPRESSED[idx].swap(0, Ordering::Relaxed);
    let mut stderr = std::io::stderr().lock();
    if suppressed > 0 {
        let _ = writeln!(
            stderr,
            "[{:.1}s] {} {} (suppressed {suppressed} similar)",
            elapsed_secs(),
            level.as_str(),
            msg
        );
    } else {
        let _ = writeln!(stderr, "[{:.1}s] {} {}", elapsed_secs(), level.as_str(), msg);
    }
}

pub fn refill_buckets() {
    for bucket in &RATE_BUCKETS {
        let current = bucket.load(Ordering::Relaxed);
        if current < 3 {
            bucket.store(current.saturating_add(1), Ordering::Relaxed);
        }
    }
}

pub fn emit_raw(level: LogLevel, msg: &str) {
    if !is_enabled(level) {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "[{:.1}s] {} {}", elapsed_secs(), level.as_str(), msg);
}

#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)*) => {
        if $crate::log::is_enabled($level) {
            let site_hash = {
                let file = file!();
                let line = line!() as usize;
                let mut h: usize = 5381;
                for b in file.bytes() {
                    h = h.wrapping_mul(33).wrapping_add(b as usize);
                }
                h.wrapping_add(line)
            };
            let msg = format!($($arg)*);
            $crate::log::rate_limited_emit(site_hash, $level, &msg);
        }
    };
}
