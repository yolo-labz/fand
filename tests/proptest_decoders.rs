//! Property-based tests for the SMC byte decoders/encoders (FR-014, FR-086).
//!
//! Verifies that `flt`/`ui8`/`ui32` round-trip correctly across the full
//! input domain. The decoders live in `src/smc/types.rs` and are exercised
//! both directly and via the higher-level read/write paths.
//!
//! Per FR-086: ≥10,000 cases per property in CI, ≥1,000,000 in nightly soak.

use proptest::prelude::*;

use fand::smc::types::{decode_flt, decode_ui32, decode_ui8, encode_flt, encode_ui32, encode_ui8};

/// Property: encode_flt + decode_flt round-trips for any finite f32.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn flt_round_trips_finite(value in any::<f32>().prop_filter("finite", |v| v.is_finite())) {
        let encoded = encode_flt(value).expect("encode finite");
        let decoded = decode_flt(&encoded, 4).expect("decode 4 bytes");
        prop_assert_eq!(decoded.to_bits(), value.to_bits(),
            "f32 round-trip failed: {} (bits={:#010X})",
            value, value.to_bits());
    }
}

/// Property: encode_flt rejects NaN.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn flt_encode_rejects_nan(_dummy in any::<u8>()) {
        let result = encode_flt(f32::NAN);
        prop_assert!(result.is_err(), "encoding NaN must fail");
    }
}

/// Property: encode_flt rejects ±infinity.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn flt_encode_rejects_inf(_dummy in any::<u8>()) {
        prop_assert!(encode_flt(f32::INFINITY).is_err());
        prop_assert!(encode_flt(f32::NEG_INFINITY).is_err());
    }
}

/// Property: decode_flt rejects NaN bit patterns from the wire.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn flt_decode_rejects_nan_wire(_dummy in any::<u8>()) {
        let nan_bytes = f32::NAN.to_le_bytes();
        let result = decode_flt(&nan_bytes, 4);
        prop_assert!(result.is_err(), "decoding NaN bit pattern must fail");
    }
}

/// Property: ui8 round-trips for every byte.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn ui8_round_trips(value in any::<u8>()) {
        let encoded = encode_ui8(value);
        let decoded = decode_ui8(&encoded, 1).expect("decode 1 byte");
        prop_assert_eq!(decoded, value);
    }
}

/// Property: ui32 round-trips for every u32.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn ui32_round_trips_be(value in any::<u32>()) {
        let encoded = encode_ui32(value);
        let decoded = decode_ui32(&encoded, 4).expect("decode 4 bytes");
        prop_assert_eq!(decoded, value);
    }
}

/// Property: decode_flt rejects wrong sizes.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn flt_decode_rejects_wrong_size(size in 0u32..32) {
        if size == 4 {
            return Ok(()); // skip the valid case
        }
        let bytes = [0u8; 32];
        let result = decode_flt(&bytes, size);
        prop_assert!(result.is_err(), "decoding flt with size={} must fail", size);
    }
}

/// Property: decode_ui8 rejects wrong sizes.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn ui8_decode_rejects_wrong_size(size in 0u32..32) {
        if size == 1 {
            return Ok(());
        }
        let bytes = [0u8; 32];
        let result = decode_ui8(&bytes, size);
        prop_assert!(result.is_err(), "decoding ui8 with size={} must fail", size);
    }
}

/// Property: decode_ui32 rejects wrong sizes.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    #[test]
    fn ui32_decode_rejects_wrong_size(size in 0u32..32) {
        if size == 4 {
            return Ok(());
        }
        let bytes = [0u8; 32];
        let result = decode_ui32(&bytes, size);
        prop_assert!(result.is_err(), "decoding ui32 with size={} must fail", size);
    }
}
