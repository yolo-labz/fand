#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SmcValueError {
    InvalidFloat,
    SizeMismatch { expected: usize, got: u32 },
}

impl core::fmt::Display for SmcValueError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidFloat => write!(f, "NaN or infinity in flt decode"),
            Self::SizeMismatch { expected, got } => {
                write!(f, "expected {expected} bytes, got {got}")
            }
        }
    }
}

impl std::error::Error for SmcValueError {}

pub fn decode_flt(bytes: &[u8], data_size: u32) -> Result<f32, SmcValueError> {
    if data_size != 4 {
        return Err(SmcValueError::SizeMismatch {
            expected: 4,
            got: data_size,
        });
    }
    let arr: [u8; 4] = [
        *bytes.first().unwrap_or(&0),
        *bytes.get(1).unwrap_or(&0),
        *bytes.get(2).unwrap_or(&0),
        *bytes.get(3).unwrap_or(&0),
    ];
    let val = f32::from_le_bytes(arr);
    if val.is_nan() || val.is_infinite() {
        return Err(SmcValueError::InvalidFloat);
    }
    Ok(val)
}

pub fn encode_flt(val: f32) -> Result<[u8; 4], SmcValueError> {
    if val.is_nan() || val.is_infinite() {
        return Err(SmcValueError::InvalidFloat);
    }
    Ok(val.to_le_bytes())
}

pub fn decode_ui8(bytes: &[u8], data_size: u32) -> Result<u8, SmcValueError> {
    if data_size != 1 {
        return Err(SmcValueError::SizeMismatch {
            expected: 1,
            got: data_size,
        });
    }
    Ok(*bytes.first().unwrap_or(&0))
}

pub fn encode_ui8(val: u8) -> [u8; 1] {
    [val]
}

pub fn decode_ui32(bytes: &[u8], data_size: u32) -> Result<u32, SmcValueError> {
    if data_size != 4 {
        return Err(SmcValueError::SizeMismatch {
            expected: 4,
            got: data_size,
        });
    }
    let arr: [u8; 4] = [
        *bytes.first().unwrap_or(&0),
        *bytes.get(1).unwrap_or(&0),
        *bytes.get(2).unwrap_or(&0),
        *bytes.get(3).unwrap_or(&0),
    ];
    Ok(u32::from_be_bytes(arr))
}

pub fn encode_ui32(val: u32) -> [u8; 4] {
    val.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flt_round_trip() {
        let values: &[f32] = &[0.0, 1.0, -1.0, 72.5, 6400.0, f32::MIN_POSITIVE, f32::MAX];
        for &v in values {
            let encoded = encode_flt(v).expect("encode");
            let decoded = decode_flt(&encoded, 4).expect("decode");
            assert!(
                (decoded - v).abs() < f32::EPSILON || decoded == v,
                "round-trip failed for {v}"
            );
        }
    }

    #[test]
    fn flt_rejects_nan() {
        let nan_bytes = f32::NAN.to_le_bytes();
        assert!(matches!(
            decode_flt(&nan_bytes, 4),
            Err(SmcValueError::InvalidFloat)
        ));
        assert!(matches!(
            encode_flt(f32::NAN),
            Err(SmcValueError::InvalidFloat)
        ));
    }

    #[test]
    fn flt_rejects_infinity() {
        let inf_bytes = f32::INFINITY.to_le_bytes();
        assert!(matches!(
            decode_flt(&inf_bytes, 4),
            Err(SmcValueError::InvalidFloat)
        ));
        assert!(matches!(
            encode_flt(f32::INFINITY),
            Err(SmcValueError::InvalidFloat)
        ));
        assert!(matches!(
            encode_flt(f32::NEG_INFINITY),
            Err(SmcValueError::InvalidFloat)
        ));
    }

    #[test]
    fn flt_wrong_size() {
        assert!(matches!(
            decode_flt(&[0; 32], 2),
            Err(SmcValueError::SizeMismatch {
                expected: 4,
                got: 2
            })
        ));
    }

    #[test]
    fn ui8_round_trip() {
        for v in [0u8, 1, 127, 255] {
            let encoded = encode_ui8(v);
            let decoded = decode_ui8(&encoded, 1).expect("decode");
            assert_eq!(decoded, v);
        }
    }

    #[test]
    fn ui32_round_trip_be() {
        for v in [0u32, 1, 0xDEAD_BEEF, u32::MAX] {
            let encoded = encode_ui32(v);
            let decoded = decode_ui32(&encoded, 4).expect("decode");
            assert_eq!(decoded, v);
        }
    }

    /// FR-042: Ground-truth endianness test with hardcoded bytes.
    ///
    /// Locks in the `ui32` endianness convention against accidental flip.
    /// Convention: big-endian, carried over from Intel-era SMCKit.
    ///
    /// If Apple Silicon SMC actually uses little-endian at the wire layer,
    /// this test passes AND `fand keys` fails the runtime `#KEY`-range
    /// check (FR-061) on first run — that check is the authoritative gate.
    #[test]
    fn ui32_ground_truth_endianness() {
        // [0x00, 0x00, 0x02, 0x34]:
        // - BE decode: 0x0000_0234 == 564
        // - LE decode: 0x3402_0000 == 872_546_304
        let bytes = [0x00_u8, 0x00, 0x02, 0x34];
        let decoded = decode_ui32(&bytes, 4).expect("decode");
        assert_eq!(decoded, 564, "decode_ui32 must be big-endian");
    }

    #[test]
    fn flt_property_based() {
        let mut rng_state: u32 = 0xDEAD_BEEF;
        for _ in 0..10_000 {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 17;
            rng_state ^= rng_state << 5;
            let bits = rng_state;
            let val = f32::from_bits(bits);
            if val.is_nan() || val.is_infinite() {
                assert!(encode_flt(val).is_err());
                continue;
            }
            let encoded = encode_flt(val).expect("encode");
            let decoded = decode_flt(&encoded, 4).expect("decode");
            assert_eq!(
                decoded.to_bits(),
                val.to_bits(),
                "round-trip failed for bits {bits:#010X}"
            );
        }
    }
}
