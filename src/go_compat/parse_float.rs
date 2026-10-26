/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! Go's `strconv.ParseFloat(s, 32)` (go1.26.10 `internal/strconv`): what goautoneg reads
//! an `Accept` q-value with.
//!
//! Go's syntax is wider than Rust's `f32::from_str` in two ways that reach a q-value: underscores
//! between digits (`1_0` is 10, even without a base prefix) and hexadecimal mantissas with a
//! binary exponent (`0x1p-1` is 0.5); and narrower in one, a sign before `nan` (`+nan` is a
//! syntax error). So the syntax is ported (`special`, `readFloat`, `underscoreOK`), hexadecimal
//! values are rounded as `atofHex` does, and only a decimal Go accepted, its underscores removed,
//! goes to `f32::from_str`, which rounds correctly as Go does.

/// Why [`parse_float32`] failed, as Go's `NumError.Err`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParseFloatError {
    /// `ErrSyntax`: Go returns 0.
    Syntax,
    /// `ErrRange`: Go returns this value, an infinity.
    Range(f32),
}

/// `strconv.ParseFloat(text, 32)`, as an `f32`.
///
/// # Errors
///
/// [`ParseFloatError::Syntax`] for anything Go does not read as a float, and
/// [`ParseFloatError::Range`] with the infinity for a value beyond `f32`.
pub fn parse_float32(text: &str) -> Result<f32, ParseFloatError> {
    let bytes = text.as_bytes();
    if let Some((value, consumed)) = special(bytes) {
        return if consumed == bytes.len() {
            Ok(value)
        } else {
            Err(ParseFloatError::Syntax)
        };
    }
    let Some(read) = read_float(bytes) else {
        return Err(ParseFloatError::Syntax);
    };
    if read.consumed != bytes.len() {
        return Err(ParseFloatError::Syntax);
    }
    let value = if read.hex {
        atof_hex32(&read)
    } else {
        // Go accepted it, so it is a decimal Rust reads too once the underscores are gone.
        text.replace('_', "")
            .parse::<f32>()
            .map_err(|_| ParseFloatError::Syntax)?
    };
    if value.is_infinite() {
        return Err(ParseFloatError::Range(value));
    }
    Ok(value)
}

/// Go's `lower`: ORs in the ASCII case bit, which lowercases a letter.
fn lower(byte: u8) -> u8 {
    byte | 0x20
}

/// Go's `special`: `inf`, `infinity` (any case, optionally signed) and `nan` (any case,
/// unsigned), with the number of bytes they take.
fn special(text: &[u8]) -> Option<(f32, usize)> {
    let (negative, rest, sign_length) = match text.first() {
        Some(b'+') => (false, &text[1..], 1),
        Some(b'-') => (true, &text[1..], 1),
        _ => (false, text, 0),
    };
    match rest.first() {
        Some(b'i' | b'I') => {
            let mut length = common_prefix_length_ignore_case(rest, b"infinity");
            // Anything longer than "inf" is fine, but without all of "infinity" only "inf" is
            // taken.
            if 3 < length && length < 8 {
                length = 3;
            }
            (length == 3 || length == 8).then(|| {
                let value = if negative {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                };
                (value, sign_length + length)
            })
        }
        Some(b'n' | b'N') if sign_length == 0 => {
            (common_prefix_length_ignore_case(rest, b"nan") == 3).then_some((f32::NAN, 3))
        }
        _ => None,
    }
}

/// Go's `commonPrefixLenIgnoreCase`: how much of `prefix` (lowercase) starts `text`, in any case.
fn common_prefix_length_ignore_case(text: &[u8], prefix: &[u8]) -> usize {
    text.iter()
        .zip(prefix)
        .take_while(|&(&byte, &want)| lower(byte) == want)
        .count()
}

/// What Go's `readFloat` read.
struct ReadFloat {
    mantissa: u64,
    exponent: i32,
    negative: bool,
    truncated: bool,
    hex: bool,
    consumed: usize,
}

/// The exponent part of Go's `readFloat`, from just after the `e` or `p`: the signed exponent, the
/// index after it and whether it held an underscore; `None` without a digit. A huge exponent is
/// capped: past 10000 the result is the same.
fn read_exponent(text: &[u8], start: usize) -> Option<(i32, usize, bool)> {
    let mut index = start;
    let mut underscores = false;
    let mut exponent_sign = 1;
    match text.get(index) {
        None => return None,
        Some(b'+') => index += 1,
        Some(b'-') => {
            index += 1;
            exponent_sign = -1;
        }
        Some(_) => {}
    }
    if !text.get(index).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut exponent = 0;
    while let Some(&byte) = text.get(index) {
        if byte == b'_' {
            underscores = true;
        } else if byte.is_ascii_digit() {
            if exponent < 10_000 {
                exponent = exponent * 10 + i32::from(byte - b'0');
            }
        } else {
            break;
        }
        index += 1;
    }
    Some((exponent * exponent_sign, index, underscores))
}

/// Go's `readFloat`: the mantissa (up to 19 decimal or 16 hexadecimal digits, the rest only
/// flagged as truncated), the exponent of its last digit, and how many bytes the number took; or
/// `None` where Go's `ok` is false.
fn read_float(text: &[u8]) -> Option<ReadFloat> {
    let mut index = 0;
    let mut underscores = false;
    let mut negative = false;
    match text.first() {
        None => return None,
        Some(b'+') => index += 1,
        Some(b'-') => {
            index += 1;
            negative = true;
        }
        Some(_) => {}
    }
    let mut base: u64 = 10;
    // 10^19 and 16^16 fit in a u64.
    let mut max_mantissa_digits = 19;
    let mut exponent_char = b'e';
    let mut hex = false;
    if index + 2 < text.len() && text[index] == b'0' && lower(text[index + 1]) == b'x' {
        base = 16;
        max_mantissa_digits = 16;
        index += 2;
        exponent_char = b'p';
        hex = true;
    }
    let (mut saw_dot, mut saw_digits) = (false, false);
    let (mut digits, mut mantissa_digits, mut decimal_point): (i32, i32, i32) = (0, 0, 0);
    let mut mantissa: u64 = 0;
    let mut truncated = false;
    while let Some(&byte) = text.get(index) {
        if byte == b'_' {
            underscores = true;
        } else if byte == b'.' {
            if saw_dot {
                break;
            }
            saw_dot = true;
            decimal_point = digits;
        } else if byte.is_ascii_digit() {
            saw_digits = true;
            if byte == b'0' && digits == 0 {
                // A leading zero.
                decimal_point -= 1;
            } else {
                digits += 1;
                if mantissa_digits < max_mantissa_digits {
                    mantissa = mantissa * base + u64::from(byte - b'0');
                    mantissa_digits += 1;
                } else if byte != b'0' {
                    truncated = true;
                }
            }
        } else if base == 16 && (b'a'..=b'f').contains(&lower(byte)) {
            saw_digits = true;
            digits += 1;
            if mantissa_digits < max_mantissa_digits {
                mantissa = mantissa * 16 + u64::from(lower(byte) - b'a' + 10);
                mantissa_digits += 1;
            } else {
                truncated = true;
            }
        } else {
            break;
        }
        index += 1;
    }
    if !saw_digits {
        return None;
    }
    if !saw_dot {
        decimal_point = digits;
    }
    if base == 16 {
        decimal_point *= 4;
        mantissa_digits *= 4;
    }
    // An optional exponent moves the point.
    if index < text.len() && lower(text[index]) == exponent_char {
        let (exponent, end, exponent_underscores) = read_exponent(text, index + 1)?;
        index = end;
        underscores |= exponent_underscores;
        decimal_point += exponent;
    } else if base == 16 {
        // A hexadecimal float must have an exponent.
        return None;
    }
    let exponent = if mantissa == 0 {
        0
    } else {
        decimal_point - mantissa_digits
    };
    if underscores && !underscore_ok(&text[..index]) {
        return None;
    }
    Some(ReadFloat {
        mantissa,
        exponent,
        negative,
        truncated,
        hex,
        consumed: index,
    })
}

/// Go's `underscoreOK`: every underscore sits between digits, or after a base prefix.
fn underscore_ok(text: &[u8]) -> bool {
    // The last character class seen: the start, a digit or base prefix, an underscore, or other.
    #[derive(PartialEq)]
    enum Saw {
        Start,
        Digit,
        Underscore,
        Other,
    }
    let mut saw = Saw::Start;
    let mut rest = text;
    if let Some((b'-' | b'+', after)) = rest.split_first() {
        rest = after;
    }
    let mut hex = false;
    let mut index = 0;
    if rest.len() >= 2 && rest[0] == b'0' && matches!(lower(rest[1]), b'b' | b'o' | b'x') {
        index = 2;
        // The base prefix counts as a digit for "underscore as digit separator".
        saw = Saw::Digit;
        hex = lower(rest[1]) == b'x';
    }
    for &byte in &rest[index..] {
        if byte.is_ascii_digit() || (hex && (b'a'..=b'f').contains(&lower(byte))) {
            saw = Saw::Digit;
            continue;
        }
        // An underscore must follow a digit.
        if byte == b'_' {
            if saw != Saw::Digit {
                return false;
            }
            saw = Saw::Underscore;
            continue;
        }
        // And be followed by one.
        if saw == Saw::Underscore {
            return false;
        }
        saw = Saw::Other;
    }
    saw != Saw::Underscore
}

/// Go's `atofHex` for `float32info`: the mantissa scaled by two to the exponent, rounded to
/// nearest even with the truncated digits as a sticky bit, saturating to infinity.
fn atof_hex32(read: &ReadFloat) -> f32 {
    const MANTISSA_BITS: u32 = 23;
    const EXPONENT_BITS: u32 = 8;
    const BIAS: i32 = -127;
    const MAX_EXPONENT: i32 = (1 << EXPONENT_BITS) + BIAS - 2;
    const MIN_EXPONENT: i32 = BIAS + 1;
    let mut mantissa = read.mantissa;
    // The mantissa is now implicitly divided by 2^MANTISSA_BITS.
    let mut exponent = read.exponent + i32::try_from(MANTISSA_BITS).unwrap_or(0);
    // A leading 1 bit, the mantissa bits, and two rounding bits, the lower one sticky.
    while mantissa != 0 && mantissa >> (MANTISSA_BITS + 2) == 0 {
        mantissa <<= 1;
        exponent -= 1;
    }
    if read.truncated {
        mantissa |= 1;
    }
    while mantissa >> (1 + MANTISSA_BITS + 2) != 0 {
        mantissa = (mantissa >> 1) | (mantissa & 1);
        exponent += 1;
    }
    // Too negative: denormalize, in the hope of a representable value.
    while mantissa > 1 && exponent < MIN_EXPONENT - 2 {
        mantissa = (mantissa >> 1) | (mantissa & 1);
        exponent += 1;
    }
    // Round with the two bottom bits, to even.
    let mut round = mantissa & 3;
    mantissa >>= 2;
    round |= mantissa & 1;
    exponent += 2;
    if round == 3 {
        mantissa += 1;
        if mantissa == 1 << (1 + MANTISSA_BITS) {
            mantissa >>= 1;
            exponent += 1;
        }
    }
    if mantissa >> MANTISSA_BITS == 0 {
        // Denormal or zero.
        exponent = BIAS;
    }
    if exponent > MAX_EXPONENT {
        // Infinity.
        mantissa = 1 << MANTISSA_BITS;
        exponent = MAX_EXPONENT + 1;
    }
    let mut bits = mantissa & ((1 << MANTISSA_BITS) - 1);
    bits |=
        u64::try_from((exponent - BIAS) & ((1 << EXPONENT_BITS) - 1)).unwrap_or(0) << MANTISSA_BITS;
    if read.negative {
        bits |= 1 << (MANTISSA_BITS + EXPONENT_BITS);
    }
    f32::from_bits(u32::try_from(bits).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `strconv.ParseFloat(s, 32)`'s result bits, as a float64, and whether it returned an error
    /// (go1.26.10, the goq program in the phase 5 commit body).
    const GO_CASES: &[(&str, u64, bool)] = &[
        ("1", 0x3ff0_0000_0000_0000, false),
        ("0", 0x0000_0000_0000_0000, false),
        ("0.5", 0x3fe0_0000_0000_0000, false),
        (".5", 0x3fe0_0000_0000_0000, false),
        ("5.", 0x4014_0000_0000_0000, false),
        ("1.e1", 0x4024_0000_0000_0000, false),
        ("1e-1", 0x3fb9_9999_a000_0000, false),
        ("1E-1", 0x3fb9_9999_a000_0000, false),
        ("+0.3", 0x3fd3_3333_4000_0000, false),
        ("-0.3", 0xbfd3_3333_4000_0000, false),
        (" 0.5", 0x0000_0000_0000_0000, true),
        ("0.5 ", 0x0000_0000_0000_0000, true),
        ("1_0", 0x4024_0000_0000_0000, false),
        ("0x1p-1", 0x3fe0_0000_0000_0000, false),
        ("0X1P-1", 0x3fe0_0000_0000_0000, false),
        ("0x_1p-1", 0x3fe0_0000_0000_0000, false),
        ("0x1_0p-4", 0x3ff0_0000_0000_0000, false),
        ("0x1p", 0x0000_0000_0000_0000, true),
        ("0x1", 0x0000_0000_0000_0000, true),
        ("0x.8p0", 0x3fe0_0000_0000_0000, false),
        ("1e39", 0x7ff0_0000_0000_0000, true),
        ("-1e39", 0xfff0_0000_0000_0000, true),
        ("1e-50", 0x0000_0000_0000_0000, false),
        ("inf", 0x7ff0_0000_0000_0000, false),
        ("+Inf", 0x7ff0_0000_0000_0000, false),
        ("-infinity", 0xfff0_0000_0000_0000, false),
        ("infx", 0x0000_0000_0000_0000, true),
        ("nan", 0x7ff8_0000_0000_0000, false),
        ("NaN", 0x7ff8_0000_0000_0000, false),
        ("+nan", 0x0000_0000_0000_0000, true),
        ("-nan", 0x0000_0000_0000_0000, true),
        ("", 0x0000_0000_0000_0000, true),
        ("q", 0x0000_0000_0000_0000, true),
        ("0.1", 0x3fb9_9999_a000_0000, false),
        ("0.3", 0x3fd3_3333_4000_0000, false),
        ("0.30000001", 0x3fd3_3333_4000_0000, false),
        ("0.7", 0x3fe6_6666_6000_0000, false),
        ("0.6", 0x3fe3_3333_4000_0000, false),
        ("1.0000001", 0x3ff0_0000_2000_0000, false),
        ("3.4028235e38", 0x47ef_ffff_e000_0000, false),
        ("3.4028236e38", 0x7ff0_0000_0000_0000, true),
        ("0x1.fffffep127", 0x47ef_ffff_e000_0000, false),
        ("0x1.ffffffp127", 0x7ff0_0000_0000_0000, true),
        ("0x1p-149", 0x36a0_0000_0000_0000, false),
        ("0x1p-150", 0x0000_0000_0000_0000, false),
        ("0x1.8p-149", 0x36b0_0000_0000_0000, false),
        ("1__0", 0x0000_0000_0000_0000, true),
        ("_1", 0x0000_0000_0000_0000, true),
        ("1_", 0x0000_0000_0000_0000, true),
        ("0x_", 0x0000_0000_0000_0000, true),
        (
            "0.000000000000000000000000000000000000000000001",
            0x36a0_0000_0000_0000,
            false,
        ),
        (
            "123456789012345678901234567890",
            0x45f8_ee91_0000_0000,
            false,
        ),
        ("0.1234567890123456789012345", 0x3fbf_9add_4000_0000, false),
    ];

    #[test]
    fn matches_go() {
        for &(input, want_bits, want_error) in GO_CASES {
            let got = parse_float32(input);
            let value = match got {
                Ok(value) | Err(ParseFloatError::Range(value)) => f64::from(value),
                Err(ParseFloatError::Syntax) => 0.0,
            };
            assert_eq!(
                (value.to_bits(), got.is_err()),
                (want_bits, want_error),
                "{input:?}: {got:?}"
            );
        }
    }
}
