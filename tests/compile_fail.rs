//! Compile-fail tests for feature 005 type-system invariants (FR-018, FR-019,
//! FR-031, FR-090, RD-08; Phase 5 / T079–T084).
//!
//! Each `tests/ui/*.rs` file is a self-contained Rust source file that is
//! EXPECTED TO FAIL TO COMPILE. The trybuild harness runs `rustc` on each
//! one and asserts the build fails. The accompanying `*.stderr` snapshot
//! files capture the expected error message; if the wording drifts, snapshot
//! regeneration is documented in the trybuild crate docs.
//!
//! These tests are the **load-bearing proof** that the safety invariants
//! cannot be violated by accident or by future contributors. If any compile-
//! fail test starts to compile, the corresponding invariant has been broken
//! and the build is rejected.

#[test]
fn compile_fail_invariants() {
    let t = trybuild::TestCases::new();
    // Each ui test asserts that calling fand's safety-critical surface in
    // a way that violates an invariant produces a compile error.
    t.compile_fail("tests/ui/write_session_no_clone.rs");
    t.compile_fail("tests/ui/write_session_no_copy.rs");
    t.compile_fail("tests/ui/smc_error_non_exhaustive.rs");
    t.compile_fail("tests/ui/writable_key_no_tuple_construct.rs");
    t.compile_fail("tests/ui/writable_key_no_inner_match.rs");
    t.compile_fail("tests/ui/clamped_rpm_no_field_construct.rs");
}
