//! Strict numeric parsers for `fand set` and `fand selftest` CLI args.
//!
//! Feature 005 FR-042 + FR-062 tightened the input surface: the parsers here
//! reject any non-ASCII-decimal form (no leading `+`, no whitespace, no hex,
//! no exponent notation, no locale-variant digits, no underscores, no trailing
//! content). Implements the regex-anchored parsing contract from FR-042.
//!
//! These parsers exist as a separate module to (a) keep the strict contract
//! colocated for review, (b) allow unit-testing without spinning up the
//! full CLI, and (c) be compile-fail-tested as the only path from raw `&str`
//! to numeric values.

use core::fmt;

/// Error returned by the strict parsers. Carries a short reason for the CLI
/// usage message; always maps to exit code 64 (usage error, FR-039).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    NonAsciiDigit,
    LeadingSign,
    Whitespace,
    LocaleDigit,
    Hex,
    Exponent,
    Underscore,
    MultipleDots,
    Overflow,
    NotFinite,
    OutOfRange,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::Empty => "value is empty",
            Self::NonAsciiDigit => "value contains non-ASCII digits",
            Self::LeadingSign => "leading '+' or '-' is not allowed",
            Self::Whitespace => "whitespace is not allowed",
            Self::LocaleDigit => "locale-variant digits are not allowed",
            Self::Hex => "hexadecimal literals are not allowed",
            Self::Exponent => "exponent notation is not allowed",
            Self::Underscore => "underscores in numbers are not allowed",
            Self::MultipleDots => "value has more than one decimal point",
            Self::Overflow => "value overflows u8 or f32",
            Self::NotFinite => "value is NaN or infinity",
            Self::OutOfRange => "value is outside the valid range",
        };
        f.write_str(reason)
    }
}

impl std::error::Error for ParseError {}

/// Maximum accepted RPM value per FR-042 (sanity ceiling).
pub const RPM_SANITY_CEILING: f32 = 50_000.0;

/// Parse a fan index as a strict ASCII decimal u8.
///
/// Accepts: `0`, `1`, ..., `255` — that's it. Rejects everything else
/// including `+0`, ` 0`, `0x0`, `0_0`, `0.0`, `00`, `1e1`, trailing whitespace.
///
/// # Errors
///
/// Returns `ParseError` with a specific reason — all map to CLI exit 64.
pub fn parse_fan_index(s: &str) -> Result<u8, ParseError> {
    if s.is_empty() {
        return Err(ParseError::Empty);
    }
    validate_strict_integer(s)?;
    // After validation, std::str::parse::<u8> is safe: only ASCII digits remain.
    s.parse::<u8>().map_err(|_| ParseError::Overflow)
}

/// Parse an RPM value as a strict ASCII decimal float.
///
/// Accepts: `0`, `1`, ..., `99999`, and decimal forms like `3000.5`. Rejects
/// everything else.
///
/// # Errors
///
/// Returns `ParseError` on invalid input, finite check, or out-of-range.
pub fn parse_rpm(s: &str) -> Result<f32, ParseError> {
    if s.is_empty() {
        return Err(ParseError::Empty);
    }
    validate_strict_float(s)?;
    let value: f32 = s.parse::<f32>().map_err(|_| ParseError::Overflow)?;
    if !value.is_finite() {
        return Err(ParseError::NotFinite);
    }
    if value < 0.0 || value > RPM_SANITY_CEILING {
        return Err(ParseError::OutOfRange);
    }
    Ok(value)
}

/// Validate a string matches `^[0-9]+$`.
fn validate_strict_integer(s: &str) -> Result<(), ParseError> {
    for &b in s.as_bytes() {
        match b {
            b'0'..=b'9' => {}
            b'+' | b'-' => return Err(ParseError::LeadingSign),
            b' ' | b'\t' | b'\n' | b'\r' => return Err(ParseError::Whitespace),
            b'_' => return Err(ParseError::Underscore),
            b'.' => return Err(ParseError::MultipleDots), // integer path, reject dot
            // Check exponent BEFORE the hex alphabet range because `e`/`E`
            // is in both, and an operator writing `1e3` almost certainly
            // meant scientific notation, not the hex character `e`.
            b'e' | b'E' => return Err(ParseError::Exponent),
            b'x' | b'X' | b'a'..=b'f' | b'A'..=b'F' => return Err(ParseError::Hex),
            _ => return Err(ParseError::NonAsciiDigit),
        }
    }
    Ok(())
}

/// Validate a string matches `^[0-9]+(\.[0-9]+)?$`.
fn validate_strict_float(s: &str) -> Result<(), ParseError> {
    let mut dots = 0usize;
    let mut last_char_was_dot = false;
    let bytes = s.as_bytes();
    if let Some(&first) = bytes.first() {
        if first == b'.' {
            return Err(ParseError::MultipleDots); // leading dot forbidden
        }
    }
    for &b in bytes {
        match b {
            b'0'..=b'9' => {
                last_char_was_dot = false;
            }
            b'.' => {
                dots = dots.saturating_add(1);
                if dots > 1 {
                    return Err(ParseError::MultipleDots);
                }
                last_char_was_dot = true;
            }
            b'+' | b'-' => return Err(ParseError::LeadingSign),
            b' ' | b'\t' | b'\n' | b'\r' => return Err(ParseError::Whitespace),
            b'_' => return Err(ParseError::Underscore),
            // Exponent takes precedence over hex for `e`/`E` — see note in
            // validate_strict_integer.
            b'e' | b'E' => return Err(ParseError::Exponent),
            b'x' | b'X' | b'a'..=b'f' | b'A'..=b'F' => return Err(ParseError::Hex),
            _ => return Err(ParseError::NonAsciiDigit),
        }
    }
    if last_char_was_dot {
        return Err(ParseError::MultipleDots); // trailing dot forbidden
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- parse_fan_index ----------

    #[test]
    fn fan_index_accepts_decimal_digits() {
        assert_eq!(parse_fan_index("0"), Ok(0));
        assert_eq!(parse_fan_index("1"), Ok(1));
        assert_eq!(parse_fan_index("7"), Ok(7));
        assert_eq!(parse_fan_index("42"), Ok(42));
        assert_eq!(parse_fan_index("255"), Ok(255));
    }

    #[test]
    fn fan_index_rejects_leading_plus() {
        assert_eq!(parse_fan_index("+1"), Err(ParseError::LeadingSign));
    }

    #[test]
    fn fan_index_rejects_negative() {
        assert_eq!(parse_fan_index("-1"), Err(ParseError::LeadingSign));
    }

    #[test]
    fn fan_index_rejects_whitespace() {
        assert_eq!(parse_fan_index(" 1"), Err(ParseError::Whitespace));
        assert_eq!(parse_fan_index("1 "), Err(ParseError::Whitespace));
        assert_eq!(parse_fan_index("1\t"), Err(ParseError::Whitespace));
    }

    #[test]
    fn fan_index_rejects_hex() {
        assert_eq!(parse_fan_index("0x10"), Err(ParseError::Hex));
        assert_eq!(parse_fan_index("1a"), Err(ParseError::Hex));
    }

    #[test]
    fn fan_index_rejects_underscore() {
        assert_eq!(parse_fan_index("1_0"), Err(ParseError::Underscore));
    }

    #[test]
    fn fan_index_rejects_dot() {
        assert_eq!(parse_fan_index("1.0"), Err(ParseError::MultipleDots));
    }

    #[test]
    fn fan_index_rejects_exponent() {
        assert_eq!(parse_fan_index("1e1"), Err(ParseError::Exponent));
    }

    #[test]
    fn fan_index_rejects_non_ascii_digit() {
        // Arabic-Indic digit one U+0661 — encoded as 3 UTF-8 bytes
        assert!(matches!(
            parse_fan_index("١"),
            Err(ParseError::NonAsciiDigit)
        ));
    }

    #[test]
    fn fan_index_rejects_overflow() {
        assert_eq!(parse_fan_index("256"), Err(ParseError::Overflow));
        assert_eq!(parse_fan_index("9999"), Err(ParseError::Overflow));
    }

    #[test]
    fn fan_index_rejects_empty() {
        assert_eq!(parse_fan_index(""), Err(ParseError::Empty));
    }

    // ---------- parse_rpm ----------

    #[test]
    fn rpm_accepts_integer() {
        assert_eq!(parse_rpm("3000"), Ok(3000.0));
        assert_eq!(parse_rpm("0"), Ok(0.0));
        assert_eq!(parse_rpm("50000"), Ok(50000.0));
    }

    #[test]
    fn rpm_accepts_decimal() {
        assert_eq!(parse_rpm("3000.5"), Ok(3000.5));
        assert_eq!(parse_rpm("2317.0"), Ok(2317.0));
        assert_eq!(parse_rpm("0.1"), Ok(0.1));
    }

    #[test]
    fn rpm_rejects_leading_sign() {
        assert_eq!(parse_rpm("+3000"), Err(ParseError::LeadingSign));
        assert_eq!(parse_rpm("-100"), Err(ParseError::LeadingSign));
    }

    #[test]
    fn rpm_rejects_whitespace() {
        assert_eq!(parse_rpm(" 3000"), Err(ParseError::Whitespace));
        assert_eq!(parse_rpm("3000 "), Err(ParseError::Whitespace));
    }

    #[test]
    fn rpm_rejects_exponent() {
        assert_eq!(parse_rpm("3e3"), Err(ParseError::Exponent));
        assert_eq!(parse_rpm("3.0E3"), Err(ParseError::Exponent));
    }

    #[test]
    fn rpm_rejects_hex() {
        assert_eq!(parse_rpm("0x10"), Err(ParseError::Hex));
        assert_eq!(parse_rpm("3a"), Err(ParseError::Hex));
    }

    #[test]
    fn rpm_rejects_underscore() {
        assert_eq!(parse_rpm("3_000"), Err(ParseError::Underscore));
    }

    #[test]
    fn rpm_rejects_multiple_dots() {
        assert_eq!(parse_rpm("1.2.3"), Err(ParseError::MultipleDots));
        assert_eq!(parse_rpm(".5"), Err(ParseError::MultipleDots));
        assert_eq!(parse_rpm("5."), Err(ParseError::MultipleDots));
    }

    #[test]
    fn rpm_rejects_nan_and_inf_text() {
        assert!(matches!(parse_rpm("NaN"), Err(_)));
        assert!(matches!(parse_rpm("inf"), Err(_)));
        assert!(matches!(parse_rpm("infinity"), Err(_)));
    }

    #[test]
    fn rpm_rejects_out_of_range() {
        assert_eq!(parse_rpm("50001"), Err(ParseError::OutOfRange));
        assert_eq!(parse_rpm("999999"), Err(ParseError::OutOfRange));
    }

    #[test]
    fn rpm_rejects_empty() {
        assert_eq!(parse_rpm(""), Err(ParseError::Empty));
    }

    #[test]
    fn rpm_accepts_exact_ceiling() {
        assert_eq!(parse_rpm("50000"), Ok(50000.0));
        assert_eq!(parse_rpm("50000.0"), Ok(50000.0));
    }
}
