//! Compile-fail: `ClampedRpm` cannot be tuple-constructed from outside the
//! defining module.
//!
//! Per FR-018 + FR-020 + SC-013: `ClampedRpm` is the only type accepted by
//! the write boundary. Its inner `u32` field is private. The ONLY public
//! construction path is `ClampedRpm::new(raw, min, max)` which always
//! applies the clamping operation (FR-016) AND honors `FAND_SAFE_MIN_RPM`
//! (FR-063). Direct tuple-construction would bypass clamping and let
//! arbitrary RPM values reach the SMC write boundary, defeating the
//! "single source of truth" invariant in FR-020.
//!
//! Expected error: `tuple struct constructor `ClampedRpm` is private`
//! OR `cannot initialize private fields`.

use fand::control::state::ClampedRpm;

fn main() {
    // The u32 inner field is private; tuple syntax MUST fail.
    let _ = ClampedRpm(5000);
}
