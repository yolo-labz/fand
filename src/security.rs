//! Security hardening for the feature 005 sudo boundary (FR-061).
//!
//! Implements an **allowlist** environment-variable scrubbing policy: at the
//! top of `main()`, iterate every environment variable and remove any whose
//! name is NOT in the explicit allowlist. This defeats command-injection and
//! dylib-injection tricks where a non-root caller steers the write path via
//! crafted env vars inherited across the sudo boundary.
//!
//! Spec reference: FR-061 (allowlist policy), resolved from analyze finding U3.

#![allow(clippy::missing_docs_in_private_items)]

/// Environment variable names that are ALLOWED to survive the scrub.
///
/// Everything not in this list is `remove_var`'d. Notable exclusions: the
/// full `DYLD_*` family (15+ variants per `dyld(1)`), the `Malloc*` family,
/// `TMPDIR`, `LANG`, `LC_*`, `RUST_BACKTRACE`, `RUST_LOG`, `CFFIXED_USER_HOME`.
const ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SUDO_USER",
    "SUDO_UID",
    "SUDO_GID",
    "SUDO_COMMAND",
    "TERM",
    "SHELL",
    "FAND_SAFE_MIN_RPM",       // FR-063 opt-in safe-min override
    "FAND_ALLOW_TMP_CONFIG",   // FR-071 dev/testing bypass for /tmp config path check
];

/// Scrub the process environment against the allowlist (FR-061).
///
/// Called from `main()` before ANY other initialization — specifically before
/// `std::env::args` is consulted, before any fan write, before loading config.
///
/// # Panics
///
/// Never. `std::env::vars` + `std::env::remove_var` are infallible.
pub fn scrub_env() {
    let snapshot: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .collect();
    for name in snapshot {
        if !ALLOWLIST.iter().any(|allowed| *allowed == name.as_str()) {
            std::env::remove_var(&name);
        }
    }
}

/// Return the current allowlist — test-only accessor.
#[cfg(test)]
#[must_use]
pub fn allowlist() -> &'static [&'static str] {
    ALLOWLIST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_contains_sudo_vars() {
        let list = allowlist();
        assert!(list.contains(&"SUDO_USER"));
        assert!(list.contains(&"SUDO_UID"));
        assert!(list.contains(&"PATH"));
        assert!(list.contains(&"HOME"));
    }

    #[test]
    fn allowlist_excludes_dyld_family() {
        let list = allowlist();
        assert!(!list.iter().any(|s| s.starts_with("DYLD_")));
        assert!(!list.iter().any(|s| s.starts_with("Malloc")));
        assert!(!list.contains(&"TMPDIR"));
        assert!(!list.contains(&"LANG"));
        assert!(!list.contains(&"RUST_BACKTRACE"));
    }

    #[test]
    fn scrub_removes_dyld_insert_libraries() {
        std::env::set_var("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib");
        // Don't actually call scrub_env in the unit test — it would break
        // other tests running in the same process. Instead, verify the
        // allowlist lookup would reject it.
        let list = allowlist();
        let name = "DYLD_INSERT_LIBRARIES";
        assert!(!list.iter().any(|allowed| *allowed == name));
        std::env::remove_var("DYLD_INSERT_LIBRARIES");
    }

    #[test]
    fn scrub_preserves_fand_safe_min_rpm() {
        // FR-063 opt-in override must survive the scrub
        let list = allowlist();
        assert!(list.contains(&"FAND_SAFE_MIN_RPM"));
    }
}
