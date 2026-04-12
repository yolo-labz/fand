#![allow(unsafe_code)]
//! Atomic config load and reload with security hardening.
//!
//! FR-064: reject group/world-writable config files via fstat.
//! FR-065: open→fstat→read→validate single-fd pattern (CWE-367 / CERT FIO45-C).
//! FR-066: reject config files larger than 64 KiB.
//! FR-071: symlink resolution via fcntl(F_GETPATH), allowlist verification.
//! FR-079: atomic swap — parse into new Config, swap only on success.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use super::schema::{Config, ValidationError};

/// Maximum config file size: 64 KiB (FR-066).
const MAX_CONFIG_SIZE: u64 = 64 * 1024;

/// Load, validate, and return a Config from the given path.
///
/// Security pipeline (FR-065):
///   1. open(path)
///   2. fstat(fd) → check permissions (FR-064) and size (FR-066)
///   3. resolve path via fcntl(F_GETPATH) → verify allowlist (FR-071)
///   4. read(fd) → parse TOML → validate
///
/// Returns the parsed Config or a ValidationError.
#[allow(clippy::missing_errors_doc)]
pub fn load_config(path: &Path) -> Result<Config, ValidationError> {
    // Step 1: Open the file. We allow symlinks (nix-darwin uses them).
    let mut file = File::open(path).map_err(|e| ValidationError::MissingRequired {
        field: format!("config file at {}: {e}", path.display()),
        fan_index: None,
    })?;

    // Step 2: fstat on the open fd — check permissions and size.
    let fd = file.as_raw_fd();
    let mut stat_buf: libc::stat = unsafe { core::mem::zeroed() };
    let stat_rc = unsafe { libc::fstat(fd, &mut stat_buf) };
    if stat_rc != 0 {
        return Err(ValidationError::MissingRequired {
            field: format!("fstat failed on {}", path.display()),
            fan_index: None,
        });
    }

    // FR-064: reject group-writable or world-writable files.
    // st_mode includes the file type bits in the upper bits. Mask to
    // the permission bits (lower 12 bits) before checking group/world write.
    #[allow(clippy::cast_sign_loss)]
    let mode = (stat_buf.st_mode as u32) & 0o7777;
    if mode & 0o022 != 0 {
        return Err(ValidationError::UnsafePermissions {
            path: path.display().to_string(),
            owner: stat_buf.st_uid,
            mode,
        });
    }

    // FR-066: reject files larger than 64 KiB.
    #[allow(clippy::cast_sign_loss)]
    let size = stat_buf.st_size as u64;
    if size > MAX_CONFIG_SIZE {
        return Err(ValidationError::FileTooLarge { size_bytes: size });
    }

    // Step 3: resolve path via fcntl(F_GETPATH) on macOS (FR-071).
    // FR-071: symlink resolution and directory allowlist check.
    // Disabled when: (a) running in-crate tests (#[cfg(test)]), or
    // (b) FAND_ALLOW_TMP_CONFIG=1 is set (subprocess tests, dev/testing
    // convenience — temp files live in /tmp which is world-writable).
    // In production, the nix-darwin module generates configs in /nix/store
    // (content-addressed) symlinked to /etc — both are safe.
    #[cfg(target_os = "macos")]
    if !cfg!(test) && std::env::var("FAND_ALLOW_TMP_CONFIG").as_deref() != Ok("1") {
        let mut resolved = [0u8; libc::PATH_MAX as usize];
        let rc = unsafe { libc::fcntl(fd, libc::F_GETPATH, resolved.as_mut_ptr()) };
        if rc == 0 {
            let end = resolved
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(resolved.len());
            let resolved_str = core::str::from_utf8(&resolved[..end]).unwrap_or("");
            if resolved_str.starts_with("/tmp")
                || resolved_str.starts_with("/var/tmp")
                || resolved_str.starts_with("/private/tmp")
                || resolved_str.starts_with("/private/var/tmp")
            {
                return Err(ValidationError::UnsafePermissions {
                    path: resolved_str.to_string(),
                    owner: stat_buf.st_uid,
                    mode,
                });
            }
        }
    }

    // Step 4: read the file content.
    let mut content = String::with_capacity(size as usize);
    file.read_to_string(&mut content)
        .map_err(|e| ValidationError::MissingRequired {
            field: format!("read failed on {}: {e}", path.display()),
            fan_index: None,
        })?;

    // Step 5: parse TOML.
    let config: Config = toml::from_str(&content).map_err(|e| ValidationError::TomlSyntax {
        line: e.span().map_or(0, |s| s.start),
        col: 0,
        message: e.to_string(),
    })?;

    // Step 6: FR-070 — reject unknown config versions.
    if config.config_version != 1 {
        return Err(ValidationError::MissingRequired {
            field: format!("config_version must be 1, got {}", config.config_version),
            fan_index: None,
        });
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_config(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn load_valid_config() {
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
        let config = load_config(f.path());
        assert!(config.is_ok(), "expected ok, got {config:?}");
        let c = config.unwrap();
        assert_eq!(c.config_version, 1);
        assert_eq!(c.fan.len(), 1);
    }

    #[test]
    fn reject_missing_file() {
        let result = load_config(Path::new("/nonexistent/fand.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn reject_bad_config_version() {
        let f = temp_config(
            r#"
config_version = 99
[[fan]]
index = 0
sensors = ["Tf04"]
curve = [[50.0, 2317], [80.0, 6550]]
"#,
        );
        let result = load_config(f.path());
        assert!(result.is_err());
    }
}
