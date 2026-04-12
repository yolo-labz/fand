//! Compile-fail: `WritableKey` cannot be constructed via tuple syntax from
//! outside the defining module.
//!
//! Per FR-019 + I2 resolution: `WritableKey` is an opaque newtype wrapping
//! a private `inner::WritableKeyInner` enum. The only construction paths
//! are the three factory methods `WritableKey::fan_mode(i)`,
//! `WritableKey::fan_target(i)`, `WritableKey::ftst()`. Direct tuple-struct
//! construction would let an attacker bypass the whitelist and write
//! arbitrary keys, defeating the FR-017 `pub(in crate::smc)` boundary.
//!
//! The compile-time `variant_count() == 3` assertion in `src/smc/keys.rs`
//! locks the inner enum to exactly three variants. THIS test ensures that
//! external code cannot use the tuple syntax to construct a fourth.
//!
//! Expected error: `cannot find type `WritableKeyInner` in module `inner``
//! OR `tuple struct constructor `WritableKey` is private`.

use fand::smc::keys::WritableKey;

fn main() {
    // Attempt to use the tuple-struct syntax. The inner field is private,
    // so this MUST fail with a privacy or no-such-constructor error.
    let _ = WritableKey(unimplemented!());
}
