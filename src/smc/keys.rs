//! SMC command bytes and the `WritableKey` opaque-newtype whitelist (FR-019).
//!
//! `WritableKey` is the ONLY way to express a write target. Public construction is
//! restricted to the three factory methods; the inner enum is in a nested private
//! module so no code outside this file can construct arbitrary variants. A
//! compile-time `variant_count() == 3` assertion locks the invariant.

// ------------------------------------------------------------------------
// SMC command bytes
// ------------------------------------------------------------------------

pub const SMC_CMD_READ_KEY: u8 = 5;
pub const SMC_CMD_WRITE_KEY: u8 = 6;
pub const SMC_CMD_GET_KEY_FROM_IDX: u8 = 8;
pub const SMC_CMD_GET_KEY_INFO: u8 = 9;

// ------------------------------------------------------------------------
// SMC type fourcc constants (for data_type comparison)
// ------------------------------------------------------------------------

pub const TYPE_FLT: u32 = u32::from_be_bytes(*b"flt ");
pub const TYPE_UI8: u32 = u32::from_be_bytes(*b"ui8 ");
pub const TYPE_UI32: u32 = u32::from_be_bytes(*b"ui32");

// ------------------------------------------------------------------------
// WritableKey — opaque newtype (FR-019)
// ------------------------------------------------------------------------

/// A whitelisted SMC key that may be written. Exactly three variants exist:
/// - `WritableKey::fan_mode(i)` → `F<i>md` (lowercase per Apple Silicon convention, verified on Mac17,2)
/// - `WritableKey::fan_target(i)` → `F<i>Tg`
/// - `WritableKey::ftst()` → `Ftst`
///
/// Construction is restricted to these three factory methods. The inner enum
/// lives in a private module so external code cannot add a fourth variant.
/// A compile-time assertion locks `variant_count() == 3`.
pub struct WritableKey(inner::WritableKeyInner);

impl WritableKey {
    /// Construct a fan-mode write key for fan index `i`.
    #[must_use]
    pub fn fan_mode(i: u8) -> Self {
        Self(inner::WritableKeyInner::FanMode(i))
    }

    /// Construct a fan-target-RPM write key for fan index `i`.
    #[must_use]
    pub fn fan_target(i: u8) -> Self {
        Self(inner::WritableKeyInner::FanTarget(i))
    }

    /// Construct the `Ftst` diagnostic-unlock write key.
    #[must_use]
    pub fn ftst() -> Self {
        Self(inner::WritableKeyInner::Ftst)
    }

    /// The fourcc this key writes to.
    #[must_use]
    pub fn fourcc(&self) -> u32 {
        match self.0 {
            // Apple Silicon lowercase convention (feature 004 live verification
            // on Mac17,2 found `F0md` not `F0Md`). docs/ARCHITECTURE.md scopes
            // the project to Apple Silicon only, so lowercase is the unambiguous
            // choice. If a future feature needs Intel Mac support, add a sibling
            // `FanModeIntel` variant with uppercase and probe-then-select.
            inner::WritableKeyInner::FanMode(i) => fan_key_fourcc(b'm', b'd', i),
            // **Feature 005 RD-08 session 4 breakthrough**: on Apple
            // Silicon M-series, the writable fan control key is `F<i>Dc`
            // (duty cycle, flt, range [0.0, 1.0]), NOT `F<i>Tg`. `F<i>Tg`
            // is a read-only alias for thermalmonitord's effective-target
            // view. Identified via `fand keys --all` + `--read F0Dc` on
            // Mac17,2: current value 0.237 at 5021 actual RPM in the
            // [2317, 6550] envelope. The `fan_target` variant name is
            // retained for spec alignment, but the emitted fourcc is now
            // `F<i>Dc` and the caller is expected to pass a duty fraction
            // in [0.0, 1.0], not a raw RPM.
            inner::WritableKeyInner::FanTarget(i) => fan_key_fourcc(b'D', b'c', i),
            inner::WritableKeyInner::Ftst => u32::from_be_bytes(*b"Ftst"),
        }
    }

    /// The SMC data type expected for this write.
    #[must_use]
    pub fn data_type(&self) -> u32 {
        match self.0 {
            inner::WritableKeyInner::FanMode(_) => TYPE_UI8,
            inner::WritableKeyInner::FanTarget(_) => TYPE_FLT,
            inner::WritableKeyInner::Ftst => TYPE_UI8,
        }
    }
}

/// Construct a fan key fourcc like `F0Md` from suffix bytes.
#[must_use]
pub(crate) fn fan_key_fourcc(c3: u8, c4: u8, index: u8) -> u32 {
    let idx_char = b'0' + index;
    u32::from_be_bytes([b'F', idx_char, c3, c4])
}

mod inner {
    /// Private inner enum — the source of truth for "how many variants exist".
    /// External code cannot name this type or match on it.
    pub(super) enum WritableKeyInner {
        FanMode(u8),
        FanTarget(u8),
        Ftst,
    }
}

/// Compile-time variant count lock. If anyone adds a 4th variant to
/// `inner::WritableKeyInner`, this `const fn` no longer exhaustively matches
/// (the match is non-exhaustive) and the build fails.
#[must_use]
const fn variant_count() -> usize {
    // An exhaustive match acts as a compile-time proof of exactly-N variants.
    // Using a dummy pattern that references every variant forces the compiler
    // to error if a new variant is added without updating this function.
    let dummy = inner::WritableKeyInner::Ftst;
    match dummy {
        inner::WritableKeyInner::FanMode(_) => 1,
        inner::WritableKeyInner::FanTarget(_) => 2,
        inner::WritableKeyInner::Ftst => 3,
    }
}

const _: () = assert!(variant_count() == 3);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_mode_fourcc() {
        let k = WritableKey::fan_mode(0);
        assert_eq!(k.fourcc(), u32::from_be_bytes(*b"F0md"));
        assert_eq!(k.data_type(), TYPE_UI8);
    }

    #[test]
    fn fan_target_fourcc() {
        let k = WritableKey::fan_target(1);
        // Apple Silicon M-series: fan target is `F<i>Dc` (duty cycle).
        // See research.md RD-08 for the discovery process.
        assert_eq!(k.fourcc(), u32::from_be_bytes(*b"F1Dc"));
        assert_eq!(k.data_type(), TYPE_FLT);
    }

    #[test]
    fn ftst_fourcc() {
        let k = WritableKey::ftst();
        assert_eq!(k.fourcc(), u32::from_be_bytes(*b"Ftst"));
        assert_eq!(k.data_type(), TYPE_UI8);
    }

    #[test]
    fn variant_count_is_three() {
        assert_eq!(variant_count(), 3);
    }

    #[test]
    fn type_constants_are_readable_ascii() {
        assert_eq!(TYPE_FLT.to_be_bytes(), *b"flt ");
        assert_eq!(TYPE_UI8.to_be_bytes(), *b"ui8 ");
        assert_eq!(TYPE_UI32.to_be_bytes(), *b"ui32");
    }
}
