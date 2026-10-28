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

//! Go's `time.Duration`: [`format()`] is `Duration.String` (`1h2m3.5s`, `1.5µs`, `0s`) and
//! [`parse`] is `time.ParseDuration`. A duration is an `i64` of nanoseconds, as Go's is; the whole
//! range prints and parses, `-2562047h47m16.854775808s` included.

use std::fmt::{self, Write as _};

const NANOS_PER_MICRO: u64 = 1_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// `time.Duration(nanos).String()`.
#[must_use]
pub fn format(nanos: i64) -> String {
    let mut out = String::with_capacity(24);
    push(&mut out, nanos);
    out
}

/// Appends `time.Duration(nanos).String()` to `out`.
pub fn push(out: &mut String, nanos: i64) {
    // Built right to left into a fixed buffer, as Go does.
    let mut buffer = [0u8; 32];
    let mut write = buffer.len();
    let mut value = nanos.unsigned_abs();

    if value < NANOS_PER_SECOND {
        // Less than a second: a smaller unit, so 1.2ms rather than 0.0012s.
        let precision;
        write -= 1;
        buffer[write] = b's';
        write -= 1;
        if value == 0 {
            out.push_str("0s");
            return;
        } else if value < NANOS_PER_MICRO {
            precision = 0;
            buffer[write] = b'n';
        } else if value < NANOS_PER_MILLI {
            precision = 3;
            // U+00B5, the micro sign, is two bytes.
            write -= 1;
            buffer[write..write + 2].copy_from_slice("µ".as_bytes());
        } else {
            precision = 6;
            buffer[write] = b'm';
        }
        (write, value) = push_fraction(&mut buffer[..write], value, precision);
        write = push_integer(&mut buffer[..write], value);
    } else {
        write -= 1;
        buffer[write] = b's';
        (write, value) = push_fraction(&mut buffer[..write], value, 9);
        // value is now whole seconds.
        write = push_integer(&mut buffer[..write], value % 60);
        value /= 60;
        if value > 0 {
            write -= 1;
            buffer[write] = b'm';
            write = push_integer(&mut buffer[..write], value % 60);
            value /= 60;
            if value > 0 {
                write -= 1;
                buffer[write] = b'h';
                write = push_integer(&mut buffer[..write], value);
            }
        }
    }

    if nanos < 0 {
        write -= 1;
        buffer[write] = b'-';
    }
    // Every byte written is ASCII or the whole micro sign.
    out.push_str(std::str::from_utf8(&buffer[write..]).unwrap_or_default());
}

/// Go's `fmtFrac`: writes the `precision` low digits of `value` as a fraction, omitting trailing
/// zeros and the point when the fraction is zero, into the end of `buffer`. Returns the index of
/// the first byte written and `value` divided by `10^precision`.
fn push_fraction(buffer: &mut [u8], mut value: u64, precision: u32) -> (usize, u64) {
    let mut write = buffer.len();
    let mut print = false;
    for _ in 0..precision {
        let digit = value % 10;
        print = print || digit != 0;
        if print {
            write -= 1;
            buffer[write] = ascii_digit(digit);
        }
        value /= 10;
    }
    if print {
        write -= 1;
        buffer[write] = b'.';
    }
    (write, value)
}

/// Go's `fmtInt`: writes `value` in decimal into the end of `buffer`, and returns the index of
/// the first byte written.
fn push_integer(buffer: &mut [u8], mut value: u64) -> usize {
    let mut write = buffer.len();
    if value == 0 {
        write -= 1;
        buffer[write] = b'0';
        return write;
    }
    while value > 0 {
        write -= 1;
        buffer[write] = ascii_digit(value % 10);
        value /= 10;
    }
    write
}

/// The ASCII digit for `digit`, which is below 10.
fn ascii_digit(digit: u64) -> u8 {
    b'0' + u8::try_from(digit).unwrap_or(0)
}

/// `1 << 63`, the bound Go's `ParseDuration` checks every step against.
const OVERFLOW: u64 = 1 << 63;

/// The error of [`parse`], worded as Go's: `time: invalid duration "10x"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDurationError {
    message: String,
    value: String,
}

impl ParseDurationError {
    fn new(message: impl Into<String>, value: &str) -> Self {
        Self {
            message: message.into(),
            value: value.to_owned(),
        }
    }
}

impl fmt::Display for ParseDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "time: {} {}",
            self.message,
            time_quote(&self.value)
        )
    }
}

impl std::error::Error for ParseDurationError {}

/// `time.ParseDuration`: a possibly signed sequence of decimal numbers, each with an optional
/// fraction and a unit (`ns`, `us`, `µs`, `μs`, `ms`, `s`, `m`, `h`), such as `300ms`, `-1.5h` or
/// `2h45m`, in nanoseconds.
///
/// # Errors
///
/// Go's error, with Go's text, for anything else: no digits, a missing or unknown unit, or a
/// value outside `i64`.
pub fn parse(value: &str) -> Result<i64, ParseDurationError> {
    let invalid = || ParseDurationError::new("invalid duration", value);
    let mut rest = value.as_bytes();
    let mut negative = false;
    if let Some(&sign) = rest.first()
        && (sign == b'-' || sign == b'+')
    {
        negative = sign == b'-';
        rest = &rest[1..];
    }
    // All that is left is "0": this is zero.
    if rest == b"0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(invalid());
    }
    let mut total: u64 = 0;
    while let Some(&first) = rest.first() {
        // The next character must be [0-9.].
        if !(first == b'.' || first.is_ascii_digit()) {
            return Err(invalid());
        }
        // [0-9]*
        let before = rest.len();
        let (mut whole, after_whole) = leading_int(rest).ok_or_else(invalid)?;
        rest = after_whole;
        let has_whole = before != rest.len();
        // (\.[0-9]*)?
        let mut fraction = 0;
        let mut scale = 1.0;
        let mut has_fraction = false;
        if rest.first() == Some(&b'.') {
            rest = &rest[1..];
            let before = rest.len();
            let after_fraction;
            (fraction, scale, after_fraction) = leading_fraction(rest);
            rest = after_fraction;
            has_fraction = before != rest.len();
        }
        if !has_whole && !has_fraction {
            // No digits, as in ".s" or "-.s".
            return Err(invalid());
        }
        // The unit runs to the next digit or point.
        let unit_end = rest
            .iter()
            .position(|&byte| byte == b'.' || byte.is_ascii_digit())
            .unwrap_or(rest.len());
        if unit_end == 0 {
            return Err(ParseDurationError::new("missing unit in duration", value));
        }
        let (unit_bytes, after_unit) = rest.split_at(unit_end);
        rest = after_unit;
        let Some(unit) = unit_nanos(unit_bytes) else {
            // The unit ends at an ASCII byte or the end, so it is whole characters.
            let unit = std::str::from_utf8(unit_bytes).unwrap_or_default();
            return Err(ParseDurationError::new(
                format!("unknown unit {} in duration", time_quote(unit)),
                value,
            ));
        };
        if whole > OVERFLOW / unit {
            return Err(invalid());
        }
        whole *= unit;
        if fraction > 0 {
            whole += fraction_nanos(fraction, unit, scale);
            if whole > OVERFLOW {
                return Err(invalid());
            }
        }
        // Go's uint64 addition wraps: 1 << 63 twice is 0, which then passes the check, so
        // "9223372036854775808ns9223372036854775808ns" parses as 0 in Go too.
        total = total.wrapping_add(whole);
        if total > OVERFLOW {
            return Err(invalid());
        }
    }
    if negative {
        // -Duration(1 << 63) wraps to the minimum, as in Go.
        return Ok(0_i64.wrapping_sub_unsigned(total));
    }
    i64::try_from(total).map_err(|_| invalid())
}

/// Go's `leadingInt`: the leading `[0-9]*` and what follows, or `None` past `1 << 63`.
fn leading_int(digits: &[u8]) -> Option<(u64, &[u8])> {
    let mut value: u64 = 0;
    let mut end = 0;
    while let Some(&byte) = digits.get(end) {
        if !byte.is_ascii_digit() {
            break;
        }
        if value > OVERFLOW / 10 {
            return None;
        }
        value = value * 10 + u64::from(byte - b'0');
        if value > OVERFLOW {
            return None;
        }
        end += 1;
    }
    Some((value, &digits[end..]))
}

/// Go's `leadingFraction`: the leading `[0-9]*` as a value and its scale (10 per digit kept), and
/// what follows. Past `1 << 63` it stops accumulating precision instead of failing.
fn leading_fraction(digits: &[u8]) -> (u64, f64, &[u8]) {
    let mut value: u64 = 0;
    let mut scale = 1.0;
    let mut overflow = false;
    let mut end = 0;
    while let Some(&byte) = digits.get(end) {
        if !byte.is_ascii_digit() {
            break;
        }
        end += 1;
        if overflow {
            continue;
        }
        if value > (OVERFLOW - 1) / 10 {
            overflow = true;
            continue;
        }
        let next = value * 10 + u64::from(byte - b'0');
        if next > OVERFLOW {
            overflow = true;
            continue;
        }
        value = next;
        scale *= 10.0;
    }
    (value, scale, &digits[end..])
}

/// Go's `uint64(float64(f) * (float64(unit) / scale))`: the nanoseconds of a fraction, through
/// float64 as Go computes them (`1.9999999999s` is `1.999999999s`).
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Go converts through float64 here, and the rounding is part of the parsed value; \
              the product is at most 3.6e12 (an hour), so the conversion back cannot saturate"
)]
fn fraction_nanos(fraction: u64, unit: u64, scale: f64) -> u64 {
    (fraction as f64 * (unit as f64 / scale)) as u64
}

/// Go's `unitMap`, in nanoseconds.
fn unit_nanos(unit: &[u8]) -> Option<u64> {
    match unit {
        b"ns" => Some(1),
        // U+00B5 (the micro sign) and U+03BC (the Greek mu).
        b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => Some(NANOS_PER_MICRO),
        b"ms" => Some(NANOS_PER_MILLI),
        b"s" => Some(NANOS_PER_SECOND),
        b"m" => Some(60 * NANOS_PER_SECOND),
        b"h" => Some(3_600 * NANOS_PER_SECOND),
        _ => None,
    }
}

/// The `time` package's own `quote`: printable ASCII as is, `"` and `\\` escaped, and every byte
/// of any other character as `\xNN`.
fn time_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        if !character.is_ascii() || character < ' ' {
            let mut encoded = [0; 4];
            for byte in character.encode_utf8(&mut encoded).bytes() {
                let _ = write!(out, "\\x{byte:02x}");
            }
        } else {
            if character == '"' || character == '\\' {
                out.push('\\');
            }
            out.push(character);
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The S4 corpus durations, with Go's `Duration.String` output.
    #[test]
    fn format_matches_go() {
        let cases = [
            (0, "0s"),
            (500_000_000, "500ms"),
            (30_000_000_000, "30s"),
            (60_000_000_000, "1m0s"),
            (3_723_500_000_000, "1h2m3.5s"),
            (1, "1ns"),
            (1_500, "1.5µs"),
            (-2_000_000_000, "-2s"),
            (i64::MIN, "-2562047h47m16.854775808s"),
            (i64::MAX, "2562047h47m16.854775807s"),
            (360_000_000_000_000, "100h0m0s"),
            (1_234_567_890, "1.23456789s"),
            (1_000_001, "1.000001ms"),
        ];
        for (nanos, want) in cases {
            assert_eq!(format(nanos), want, "format({nanos})");
        }
    }

    /// Go's nanoseconds and `Duration.String`, or its error text.
    type GoResult = Result<(i64, &'static str), &'static str>;

    /// S5's 49 inputs, with Go's `time.ParseDuration` results and their `Duration.String`
    /// (go1.26.10), or its error text.
    const PARSE_CORPUS: [(&str, GoResult); 49] = [
        ("10s", Ok((10_000_000_000, "10s"))),
        ("1h2m3.5s", Ok((3_723_500_000_000, "1h2m3.5s"))),
        (".5s", Ok((500_000_000, "500ms"))),
        ("5.s", Ok((5_000_000_000, "5s"))),
        (".s", Err("time: invalid duration \".s\"")),
        ("1us", Ok((1000, "1µs"))),
        ("1µs", Ok((1000, "1µs"))),
        ("1μs", Ok((1000, "1µs"))),
        ("1ns", Ok((1, "1ns"))),
        ("1ms", Ok((1_000_000, "1ms"))),
        ("-1s", Ok((-1_000_000_000, "-1s"))),
        ("+1s", Ok((1_000_000_000, "1s"))),
        ("-0", Ok((0, "0s"))),
        ("0", Ok((0, "0s"))),
        ("00", Err("time: missing unit in duration \"00\"")),
        ("0s", Ok((0, "0s"))),
        ("1e3s", Err("time: unknown unit \"e\" in duration \"1e3s\"")),
        ("1.5h", Ok((5_400_000_000_000, "1h30m0s"))),
        ("10", Err("time: missing unit in duration \"10\"")),
        ("1h2m", Ok((3_720_000_000_000, "1h2m0s"))),
        ("1.5s", Ok((1_500_000_000, "1.5s"))),
        ("-5s", Ok((-5_000_000_000, "-5s"))),
        (
            "1h 2m",
            Err("time: unknown unit \"h \" in duration \"1h 2m\""),
        ),
        ("1H", Err("time: unknown unit \"H\" in duration \"1H\"")),
        ("1S", Err("time: unknown unit \"S\" in duration \"1S\"")),
        (
            "2562047h47m16.854775807s",
            Ok((9_223_372_036_854_775_807, "2562047h47m16.854775807s")),
        ),
        (
            "2562047h47m16.854775808s",
            Err("time: invalid duration \"2562047h47m16.854775808s\""),
        ),
        (
            "-2562047h47m16.854775808s",
            Ok((-9_223_372_036_854_775_808, "-2562047h47m16.854775808s")),
        ),
        (
            "-2562047h47m16.854775809s",
            Err("time: invalid duration \"-2562047h47m16.854775809s\""),
        ),
        (
            "9223372036854775807ns",
            Ok((9_223_372_036_854_775_807, "2562047h47m16.854775807s")),
        ),
        (
            "9223372036854775808ns",
            Err("time: invalid duration \"9223372036854775808ns\""),
        ),
        ("0.0000000001s", Ok((0, "0s"))),
        ("1.0000000001s", Ok((1_000_000_000, "1s"))),
        ("3000000h", Err("time: invalid duration \"3000000h\"")),
        (
            "99999999999999999999s",
            Err("time: invalid duration \"99999999999999999999s\""),
        ),
        ("1.9999999999s", Ok((1_999_999_999, "1.999999999s"))),
        ("0.1ns", Ok((0, "0s"))),
        ("1.5ns", Ok((1, "1ns"))),
        ("", Err("time: invalid duration \"\"")),
        ("s", Err("time: invalid duration \"s\"")),
        ("-", Err("time: invalid duration \"-\"")),
        ("1s2", Err("time: missing unit in duration \"1s2\"")),
        ("1m1m", Ok((120_000_000_000, "2m0s"))),
        ("1s1h", Ok((3_601_000_000_000, "1h0m1s"))),
        ("01s", Ok((1_000_000_000, "1s"))),
        (
            "1_000s",
            Err("time: unknown unit \"_\" in duration \"1_000s\""),
        ),
        (" 1s", Err("time: invalid duration \" 1s\"")),
        ("1s ", Err("time: unknown unit \"s \" in duration \"1s \"")),
        ("1.5.5s", Err("time: missing unit in duration \"1.5.5s\"")),
    ];

    #[test]
    fn parse_matches_go_on_the_s5_corpus() {
        for (input, want) in PARSE_CORPUS {
            let got = parse(input).map(|nanos| (nanos, format(nanos)));
            match (got, want) {
                (Ok((nanos, shown)), Ok((want_nanos, want_shown))) => {
                    assert_eq!(
                        (nanos, shown.as_str()),
                        (want_nanos, want_shown),
                        "{input:?}"
                    );
                }
                (Err(error), Err(want_error)) => {
                    assert_eq!(error.to_string(), want_error, "{input:?}");
                }
                (got, want) => panic!("{input:?}: got {got:?}, Go gave {want:?}"),
            }
        }
    }

    /// The running total wraps at `1 << 63 + 1 << 63` as Go's `uint64` does, so the sum of two
    /// out-of-range components is 0, and one more nanosecond is still refused (go1.26.10).
    #[test]
    fn parse_wraps_the_total_as_go_does() {
        assert_eq!(parse("9223372036854775808ns9223372036854775808ns"), Ok(0));
        assert_eq!(
            parse("9223372036854775808ns1ns").unwrap_err().to_string(),
            "time: invalid duration \"9223372036854775808ns1ns\""
        );
    }

    /// Go's `quote` escapes every byte of a non-ASCII or control character, and the quote and
    /// backslash.
    #[test]
    fn errors_quote_the_value_as_go_does() {
        let error = parse("1\u{e9}\"\\\x7f\t").map(|_| ()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "time: unknown unit \"\\xc3\\xa9\\\"\\\\\x7f\\x09\" in duration \"1\\xc3\\xa9\\\"\\\\\x7f\\x09\""
        );
    }
}
