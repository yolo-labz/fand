//! Config reload for SIGHUP handler (FR-039..043, FR-079..082).
//!
//! Atomic swap pattern: parse into a new Config, validate fully, and
//! only replace the active config on complete success. The previous
//! config remains in memory as a fallback (CVE-2017-7652 Mosquitto
//! pattern — see research.md RD-05).
//!
//! FR-080: SIGHUP reload is naturally rate-limited to once per tick
//! (≤500ms) because the `reload_requested` AtomicBool is checked at
//! the top of each tick iteration, not in a tight loop. This implicit
//! throttling is sufficient to prevent CVE-2020-21469-class resource
//! exhaustion.
//!
//! FR-081: SIGHUP processing is deferred to tick boundaries — the
//! signal thread sets the AtomicBool, and the tick loop checks it at
//! the START of the next tick, before any sensor reads or writes.

use std::path::Path;

use super::load::load_config;
use super::schema::{Config, ValidationError};
use super::validate;

/// Attempt to reload the config file. Returns the new Config on success,
/// or the validation errors on failure. The caller is responsible for
/// logging and deciding whether to swap.
///
/// FR-079: this function parses into a NEW Config and validates fully.
/// The caller's active config is untouched until a successful return.
#[allow(clippy::missing_errors_doc)]
pub fn reload_config(path: &Path) -> Result<Config, ReloadError> {
    let config = load_config(path).map_err(ReloadError::Load)?;
    let errors = validate::validate(&config);
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(ReloadError::Validation(errors))
    }
}

/// Error from a reload attempt.
#[derive(Debug)]
pub enum ReloadError {
    /// Config file could not be opened/read/parsed.
    Load(ValidationError),
    /// Config parsed but failed validation.
    Validation(Vec<ValidationError>),
}

impl core::fmt::Display for ReloadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Load(e) => write!(f, "config load failed: {e}"),
            Self::Validation(errs) => {
                write!(f, "config validation failed:")?;
                for e in errs {
                    write!(f, " {e};")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ReloadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_config(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        f
    }

    #[test]
    fn reload_valid_config_succeeds() {
        let f = temp_config(
            r#"
config_version = 1
poll_interval_ms = 500
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#,
        );
        let result = reload_config(f.path());
        assert!(result.is_ok());
    }

    #[test]
    fn reload_invalid_toml_fails() {
        let f = temp_config("this is not valid TOML {{{{");
        let result = reload_config(f.path());
        assert!(result.is_err());
    }

    #[test]
    fn reload_bad_validation_fails() {
        let f = temp_config(
            r#"
config_version = 1
poll_interval_ms = 500
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[80.0, 6550]]
"#,
        );
        let result = reload_config(f.path());
        assert!(matches!(result, Err(ReloadError::Validation(_))));
    }

    #[test]
    fn reload_missing_file_fails() {
        let result = reload_config(Path::new("/no/such/file.toml"));
        assert!(matches!(result, Err(ReloadError::Load(_))));
    }
}
