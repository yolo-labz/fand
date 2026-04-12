//! Compile-fail: `WriteSession` does NOT derive `Clone`.
//!
//! The three-connection model (FR-090, I1 resolution) requires that exactly
//! one `WriteSession` exists per process. Cloning the session would duplicate
//! ownership of the main `SmcConnection`, the `FlockGuard`, and the signal
//! thread join handle — all of which are RAII resources whose drop semantics
//! must run exactly once. The compiler enforces this by refusing to derive
//! `Clone` (the inner `SmcConnection` is `!Clone + !Copy` per feature 004
//! FR-056).
//!
//! Expected error: `the trait bound `WriteSession: Clone` is not satisfied`.

use fand::smc::write_session::WriteSession;

fn require_clone<T: Clone>(_: &T) {}

fn main() {
    let session = WriteSession::acquire().expect("acquire");
    require_clone(&session);
}
