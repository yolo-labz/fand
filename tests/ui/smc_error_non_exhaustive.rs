//! Compile-fail: external crates cannot exhaustively match on `SmcError`.
//!
//! Per FR-031 + FR-098 stability contract: adding a new variant to `SmcError`
//! is a non-breaking change because the enum is `#[non_exhaustive]`. External
//! consumers MUST branch on `error_code()` (the stable string), not on a
//! match arm. The compiler enforces this by requiring a wildcard `_ =>` arm
//! whenever `SmcError` is matched from outside the defining crate.
//!
//! This file deliberately matches WITHOUT a wildcard arm and expects a
//! "non-exhaustive patterns" error.
//!
//! Expected error: `non-exhaustive patterns: `_` not covered`.

use fand::smc::ffi::SmcError;

fn main() {
    let err: SmcError = SmcError::ServiceNotFound;
    // This match must FAIL because SmcError is #[non_exhaustive] and we
    // do not have a wildcard arm.
    match err {
        SmcError::ServiceNotFound => {}
    }
}
