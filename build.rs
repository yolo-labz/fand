//! Reproducible-build support for fand — FR-077.
//!
//! This build script:
//!
//! 1. Honors `SOURCE_DATE_EPOCH` — if set, its value becomes the
//!    `FAND_BUILD_EPOCH` env var injected into the crate at compile time.
//!    Callers can embed it as a build-date stamp without introducing
//!    non-determinism.
//!
//! 2. Captures the git commit hash (`FAND_GIT_REV`) so the binary knows
//!    what it was built from. If the build runs outside a git checkout
//!    (e.g., a cargo-vendor tarball), the stamp falls back to the string
//!    `"unknown"`.
//!
//! 3. Emits `cargo:rerun-if-env-changed` directives so cargo re-runs the
//!    build script when the relevant environment drifts.
//!
//! The script does NOT link against any C library, does NOT touch the
//! filesystem beyond `git`, and does NOT produce a `-l`/`-L` flag.
//! It is purely an env-var injector.

use std::env;
use std::process::Command;

fn main() {
    // 1. SOURCE_DATE_EPOCH — passthrough.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let epoch = env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=FAND_BUILD_EPOCH={epoch}");

    // 2. Git commit hash — best-effort.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    let rev = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FAND_GIT_REV={rev}");

    // 3. Hermeticity hints — forbid the build script from picking up
    //    environment that leaks into the output.
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_PROFILE_RELEASE_CODEGEN_UNITS");
}
