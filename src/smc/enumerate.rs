//! Fan enumeration: `FNum`, `F<i>Mn/Mx/Sf/Ac`, mode-key probing (FR-016/T032).
//!
//! Implements the `fand keys` P1 user story — reads fan count + per-fan
//! metadata in a byte-perfect, bounds-checked, typed manner. Never panics.
//! Returns a `Vec<Fan>` with one entry per physical fan. Handles the
//! fanless-Air case (`FNum == 0`) by returning `Ok(vec![])`.

use crate::smc::ffi::{SmcConnection, SmcError};

/// FR-061 runtime endianness gate: a plausible SMC reports at most this
/// many keys. Any value outside `[1, MAX_PLAUSIBLE_KEYS]` means the
/// `ui32` decoder endianness is inverted.
///
/// Empirical floor: M-series SMCs report ~2700-4000 keys. The inverted-LE
/// interpretation of any legitimate count would be >= 2^24, which is far
/// outside this envelope. 8000 is a safe ceiling that leaves room for
/// future Apple Silicon revisions while still catching an endianness flip.
const MAX_PLAUSIBLE_KEYS: u32 = 8000;

/// Metadata for a single physical fan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fan {
    /// Zero-based fan index (0..FNum).
    pub index: u8,
    /// Minimum RPM allowed by the SMC (`F<i>Mn`, flt).
    pub min_rpm: f32,
    /// Maximum RPM allowed by the SMC (`F<i>Mx`, flt).
    pub max_rpm: f32,
    /// Safe / default RPM (`F<i>Sf`, flt). `None` if the key is absent.
    pub safe_rpm: Option<f32>,
    /// Current actual RPM (`F<i>Ac`, flt).
    pub actual_rpm: f32,
    /// Mode-key fourcc that was probed successfully (`F<i>md` or `F<i>Md`).
    /// Stored as BE fourcc for display.
    pub mode_key: u32,
}

/// Read `#KEY` — the total SMC key count. This is the first `ui32` read
/// the daemon issues and serves as the runtime endianness gate (FR-061).
///
/// # Errors
/// - Any `read_u32` error.
/// - `SmcError::TypeMismatch` (re-used for the endianness failure) if the
///   decoded count is outside `[1, 2000]`.
pub fn read_key_count(conn: &mut SmcConnection) -> Result<u32, SmcError> {
    let fourcc = u32::from_be_bytes(*b"#KEY");
    let count = conn.read_u32(fourcc)?;
    if count == 0 || count > MAX_PLAUSIBLE_KEYS {
        // FR-061: the value is implausible — the decoder endianness is
        // almost certainly inverted. Emit the dedicated diagnostic error
        // that renders the raw u32 (not via fourcc_to_str).
        return Err(SmcError::EndiannessUnplausible { fourcc, got: count });
    }
    Ok(count)
}

/// Enumerate all fans exposed by the SMC.
///
/// Reads `FNum` (ui8). For each index `i` in `0..FNum`, reads:
/// - `F<i>Mn` (min_rpm, flt)
/// - `F<i>Mx` (max_rpm, flt)
/// - `F<i>Sf` (safe_rpm, flt, optional — swallowed on KeyNotFound)
/// - `F<i>Ac` (actual_rpm, flt)
/// - `F<i>md` then `F<i>Md` as mode-key probe (KeyNotFound → try next)
///
/// # Errors
/// - Any IOKit or SMC read error.
/// - Refuses to start if `F<i>Mn` or `F<i>Mx` is unreadable for any
///   configured fan (the daemon cannot safely run without bounds).
///
/// # Behaviour
/// - Returns `Ok(vec![])` if `FNum == 0` (fanless Air, FR-068).
pub fn enumerate_fans(conn: &mut SmcConnection) -> Result<Vec<Fan>, SmcError> {
    // First ui32 read acts as the endianness gate — do this before FNum
    // so a flipped decoder fails fast with a clear diagnostic.
    let _key_count = read_key_count(conn)?;

    let fan_count_key = u32::from_be_bytes(*b"FNum");
    let fan_count = match conn.read_u8(fan_count_key) {
        Ok(n) => n,
        // FR-068: fanless chassis (M1/M2 Air) — FNum is either absent or
        // reads as 0. Both paths collapse to "no fans" → empty vec.
        Err(SmcError::KeyNotFound(_)) => 0,
        Err(e) => return Err(e),
    };

    if fan_count == 0 {
        return Ok(Vec::new());
    }

    let mut fans = Vec::with_capacity(fan_count as usize);

    for i in 0..fan_count {
        let min_rpm = conn.read_f32(fan_key(i, b'M', b'n'))?;
        let max_rpm = conn.read_f32(fan_key(i, b'M', b'x'))?;
        let safe_rpm = match conn.read_f32(fan_key(i, b'S', b'f')) {
            Ok(v) => Some(v),
            Err(SmcError::KeyNotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let actual_rpm = conn.read_f32(fan_key(i, b'A', b'c'))?;
        let mode_key = probe_mode_key(conn, i)?;

        fans.push(Fan {
            index: i,
            min_rpm,
            max_rpm,
            safe_rpm,
            actual_rpm,
            mode_key,
        });
    }

    Ok(fans)
}

/// Construct a `F<i><c3><c4>` fourcc, e.g. `F0Mn`.
fn fan_key(index: u8, c3: u8, c4: u8) -> u32 {
    let idx_char = b'0' + index;
    u32::from_be_bytes([b'F', idx_char, c3, c4])
}

/// Probe the mode key for fan `i`. Tries lowercase `F<i>md` first (some
/// Apple Silicon SoCs use that casing), falls back to uppercase `F<i>Md`.
/// Returns the fourcc that responded successfully to a `read_key_info`
/// call (we don't care about the value — only the probe).
fn probe_mode_key(conn: &mut SmcConnection, i: u8) -> Result<u32, SmcError> {
    let lowercase = fan_key(i, b'm', b'd');
    match conn.read_key_info(lowercase) {
        Ok(_) => return Ok(lowercase),
        Err(SmcError::KeyNotFound(_)) => {}
        Err(e) => return Err(e),
    }
    let uppercase = fan_key(i, b'M', b'd');
    match conn.read_key_info(uppercase) {
        Ok(_) => Ok(uppercase),
        Err(SmcError::KeyNotFound(_)) => Err(SmcError::KeyNotFound(uppercase)),
        Err(e) => Err(e),
    }
}

// T091: miri excluded — enumerate tests open real AppleSMC connections.
#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;

    #[test]
    fn fan_key_fourcc_is_be() {
        // F0Mn = 0x46304D6E
        let expected = u32::from_be_bytes([b'F', b'0', b'M', b'n']);
        assert_eq!(fan_key(0, b'M', b'n'), expected);
    }

    #[test]
    fn fan_key_index_encoding() {
        assert_eq!(fan_key(1, b'M', b'x'), u32::from_be_bytes(*b"F1Mx"));
        assert_eq!(fan_key(2, b'A', b'c'), u32::from_be_bytes(*b"F2Ac"));
        assert_eq!(fan_key(0, b'm', b'd'), u32::from_be_bytes(*b"F0md"));
    }
}
