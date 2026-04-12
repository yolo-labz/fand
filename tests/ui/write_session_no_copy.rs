//! Compile-fail: `WriteSession` does NOT derive `Copy`.
//!
//! See `write_session_no_clone.rs` for the rationale. `Copy` is a stricter
//! constraint than `Clone` and is also forbidden by the three-connection model.
//!
//! Expected error: `the trait bound `WriteSession: Copy` is not satisfied`.

use fand::smc::write_session::WriteSession;

fn require_copy<T: Copy>(_: &T) {}

fn main() {
    let session = WriteSession::acquire().expect("acquire");
    require_copy(&session);
}
