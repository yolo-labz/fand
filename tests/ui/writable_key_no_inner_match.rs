//! Compile-fail: external code cannot match on the inner `WritableKeyInner`
//! enum because the `inner` module is `mod inner` (not `pub mod inner`).
//!
//! Per FR-019: the `inner::WritableKeyInner` enum is private to `src/smc/keys.rs`.
//! External callers can only use the three factory methods + the public
//! `fourcc()` and `data_type()` accessors. They cannot pattern-match on the
//! variant, which would otherwise let them branch on internal state.
//!
//! Expected error: `cannot find type `WritableKeyInner` in module `inner``
//! OR `module `inner` is private`.

use fand::smc::keys::WritableKey;

// Try to name the inner type. The `inner` module is `pub(super)`-scoped
// (only `src/smc/keys.rs` itself can name it), so this import MUST fail.
use fand::smc::keys::inner::WritableKeyInner;

fn main() {
    let key = WritableKey::ftst();
    // We can't even reach this — the import above already fails.
    let _ = key;
}
