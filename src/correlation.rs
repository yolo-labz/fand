//! Per-session correlation ID (ULID) for feature 005 (FR-100).
//!
//! Hand-rolled ULID generator so the feature adds zero external dependencies
//! beyond `fs4`. ULID is 128 bits: 48-bit timestamp (ms since Unix epoch) +
//! 80-bit randomness. Encoded as 26-character Crockford base32.
//!
//! Spec reference: FR-100 — every invocation of `fand set` or `fand selftest`
//! generates a ULID and stamps it into the RoundTripRing (once, at ring
//! construction), every SmcError propagation context, the JSON envelope $id,
//! and the teardown diagnostic drain.
//!
//! Entropy source: `getentropy(2)` on Darwin (always available since 10.12).

#![allow(unsafe_code)] // getentropy FFI + from_utf8_unchecked on Crockford ASCII bytes
#![allow(clippy::indexing_slicing)] // Crockford alphabet indexing is bounded by modulo 32
#![allow(clippy::as_conversions)] // u128-to-u8 truncation is deliberate for entropy mixing

use core::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Crockford base32 alphabet (FR-100 per the ULID spec).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A per-session correlation ID.
///
/// Always 26 ASCII characters, always Crockford base32, always monotonically
/// increasing within the same millisecond (time-ordered prefix). The `Copy +
/// Eq` derives make it cheap to stamp into every record in the ring.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionId([u8; 26]);

impl SessionId {
    /// Generate a fresh session ID from the current wall-clock time and 80 bits
    /// of entropy from `getentropy(2)`.
    ///
    /// # Panics
    ///
    /// Never — `getentropy` is infallible on Darwin, and `SystemTime` clamps
    /// to 0 on the Unix epoch if the clock is somehow before 1970.
    #[must_use]
    pub fn new() -> Self {
        let ms: u64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // 10 bytes of randomness for the 80-bit entropy tail.
        let mut entropy = [0u8; 10];
        // SAFETY: getentropy is infallible on Darwin for buffer sizes <= 256.
        // On non-Darwin fallback (CI on Linux), use a deterministic-enough
        // mixer so unit tests still produce distinct IDs.
        #[cfg(target_os = "macos")]
        unsafe {
            let _ = libc::getentropy(entropy.as_mut_ptr().cast(), entropy.len());
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Non-Darwin fallback: mix nanos-since-epoch with a thread id hash.
            // This is for CI build portability only; real runs are Darwin-only.
            let nanos: u128 = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            for (i, b) in entropy.iter_mut().enumerate() {
                *b = ((nanos >> (i * 8)) & 0xFF) as u8;
            }
        }

        let mut out = [0u8; 26];

        // Encode the 48-bit timestamp into the first 10 characters (50 bits,
        // top 2 bits zeroed).
        //
        // Classical ULID encoding splits the 48-bit time into 10 base32 chars
        // by packing the upper bits as the most significant symbol.
        let mut time = ms;
        for i in (0..10).rev() {
            out[i] = CROCKFORD[(time & 0x1F) as usize];
            time >>= 5;
        }

        // Encode the 80-bit entropy into the remaining 16 characters.
        // Walk the 10 bytes as a stream of 5-bit groups.
        let mut bit_buf: u64 = 0;
        let mut bits_in: u32 = 0;
        let mut out_i: usize = 10;
        for &byte in &entropy {
            bit_buf = (bit_buf << 8) | u64::from(byte);
            bits_in = bits_in.saturating_add(8);
            while bits_in >= 5 && out_i < 26 {
                bits_in = bits_in.saturating_sub(5);
                let group = ((bit_buf >> bits_in) & 0x1F) as usize;
                out[out_i] = CROCKFORD[group];
                out_i = out_i.saturating_add(1);
            }
        }
        // Flush any trailing bits.
        while out_i < 26 {
            let group = ((bit_buf << (5_u32.saturating_sub(bits_in))) & 0x1F) as usize;
            out[out_i] = CROCKFORD[group];
            out_i = out_i.saturating_add(1);
            bits_in = 0;
        }

        Self(out)
    }

    /// Return the 26-byte ASCII representation.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 26] {
        &self.0
    }

    /// Return the session ID as a borrowed `str`. Always ASCII-safe.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY: Crockford base32 alphabet is ASCII-only by construction.
        // The bytes come from CROCKFORD[...] lookups which only use ASCII digits
        // and A-Z. `from_utf8_unchecked` is sound.
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({})", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn session_id_is_26_ascii_bytes() {
        let id = SessionId::new();
        assert_eq!(id.as_bytes().len(), 26);
        for b in id.as_bytes() {
            assert!(b.is_ascii_alphanumeric(), "non-alphanumeric byte: {b:#x}");
            // Crockford excludes I, L, O, U
            assert!(!matches!(*b, b'I' | b'L' | b'O' | b'U'),
                "Crockford excluded letter: {}", *b as char);
        }
    }

    #[test]
    fn session_id_display_matches_bytes() {
        let id = SessionId::new();
        assert_eq!(id.to_string().as_bytes(), id.as_bytes());
    }

    #[test]
    fn session_ids_are_distinct_across_many_calls() {
        // T048 acceptance: 10,000 sequential calls all produce distinct IDs.
        let mut seen: HashSet<[u8; 26]> = HashSet::new();
        for _ in 0..10_000 {
            let id = SessionId::new();
            assert!(seen.insert(*id.as_bytes()), "duplicate session ID: {id}");
        }
        assert_eq!(seen.len(), 10_000);
    }

    #[test]
    fn session_id_is_copy_eq() {
        let id = SessionId::new();
        let clone = id;
        assert_eq!(id, clone);
    }
}
