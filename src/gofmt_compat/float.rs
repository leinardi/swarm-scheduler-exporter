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

//! Go's shortest float formatting.
//!
//! `strconv.FormatFloat(f, 'g', -1, 64)` takes the shortest decimal digits that round-trip, with
//! the decimal point position `dp` (the value is `0.d × 10^dp`), and writes them as `%e` when the
//! exponent `dp - 1` is below -4 or at least 6, otherwise as `%f`. `%e` writes every digit and an
//! exponent with a sign and at least two digits (`1e+06`, `1e-05`).
//!
//! The digits come from Rust's `{:e}`, with one correction. When the value lies exactly halfway
//! between two shortest candidates, Go picks the even last digit, and `{:e}` rounds up (Go writes
//! `2.1717180002771212e+15` where `{:e}` gives `…213e15`). So `{:e}` only supplies the digit
//! count, and the digits are recomputed at that precision with `{:.N$e}`, which rounds the exact
//! binary value half to even. Checked against Go on 250,034 values once in the S2 spike, and on
//! 2,000 more (S4) by the golden tests.

use std::fmt::Write as _;

/// Go's `float64(x)` of an integer of any width: the nearest `f64`, ties to even, which is what
/// Rust's `as` does for every integer type.
#[expect(
    clippy::cast_precision_loss,
    reason = "this is Go's conversion: above 2^53 the rounding is the value Go writes"
)]
pub fn from_int(value: impl Into<i128>) -> f64 {
    value.into() as f64
}

/// `strconv.FormatFloat(value, 'g', -1, 64)`.
#[must_use]
pub fn format_g(value: f64) -> String {
    let mut out = String::with_capacity(24);
    push_g(&mut out, value);
    out
}

/// Appends `strconv.FormatFloat(value, 'g', -1, 64)` to `out`.
pub fn push_g(out: &mut String, value: f64) {
    if value.is_nan() {
        out.push_str("NaN");
        return;
    }
    if value.is_infinite() {
        out.push_str(if value > 0.0 { "+Inf" } else { "-Inf" });
        return;
    }
    if value.is_sign_negative() {
        out.push('-');
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    let (digits, dp) = shortest_digits(value.abs());
    let exponent = dp - 1;
    if (-4..6).contains(&exponent) {
        push_f(out, &digits, dp);
    } else {
        push_e(out, &digits, dp, ExponentDigits::AtLeastTwo);
    }
}

/// encoding/json's float64 encoding, as slog's JSON handler gets it through `json.Marshal`:
/// `%f` unless the magnitude is below 1e-6 or at least 1e21, then `%e` with a one-digit negative
/// exponent kept short (`1e-7`, not `1e-07`). NaN and the infinities are not JSON:
/// [`UnsupportedValue`] carries encoding/json's error, whose text slog logs instead.
///
/// # Errors
///
/// [`UnsupportedValue`] for NaN, `+Inf` and `-Inf`.
pub fn format_json(value: f64) -> Result<String, UnsupportedValue> {
    if !value.is_finite() {
        return Err(UnsupportedValue(value));
    }
    let mut out = String::with_capacity(24);
    if value.is_sign_negative() {
        out.push('-');
    }
    if value == 0.0 {
        out.push('0');
        return Ok(out);
    }
    let abs = value.abs();
    let (digits, dp) = shortest_digits(abs);
    if (1e-6..1e21).contains(&abs) {
        push_f(&mut out, &digits, dp);
    } else {
        // Go formats with %e and then turns e-09 into e-9; e+21 and up have two digits anyway.
        push_e(&mut out, &digits, dp, ExponentDigits::ShortNegative);
    }
    Ok(out)
}

/// encoding/json's `UnsupportedValueError` for a float: `json: unsupported value: NaN`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnsupportedValue(pub f64);

impl std::fmt::Display for UnsupportedValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "json: unsupported value: {}", format_g(self.0))
    }
}

impl std::error::Error for UnsupportedValue {}

/// How many digits a `%e` exponent gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExponentDigits {
    /// strconv: at least two (`e+06`, `e-07`).
    AtLeastTwo,
    /// encoding/json: one for a negative exponent below 10 (`e-7`), otherwise as strconv.
    ShortNegative,
}

/// The shortest round-trip decimal digits of a finite, positive `abs` (ASCII, no leading or
/// trailing zero) and the decimal point position: `abs = 0.digits × 10^dp`.
fn shortest_digits(abs: f64) -> (Vec<u8>, i32) {
    let shortest = format!("{abs:e}");
    let precision = shortest
        .split_once('e')
        .map_or(shortest.as_str(), |(mantissa, _)| mantissa)
        .bytes()
        .filter(u8::is_ascii_digit)
        .count()
        .saturating_sub(1);
    let exact = format!("{abs:.precision$e}");
    let (mantissa, exponent) = exact.split_once('e').unwrap_or((exact.as_str(), "0"));
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    // {:.N$e} writes exactly N + 1 digits; the shortest ones carry no trailing zero beyond the
    // first, which Go would not write either.
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    let exponent: i32 = exponent.parse().unwrap_or(0);
    (digits, exponent + 1)
}

/// `%e` with every digit: `d.ddde±xx`.
fn push_e(out: &mut String, digits: &[u8], dp: i32, exponent_digits: ExponentDigits) {
    let Some((&first, rest)) = digits.split_first() else {
        return;
    };
    out.push(char::from(first));
    if !rest.is_empty() {
        out.push('.');
        out.extend(rest.iter().copied().map(char::from));
    }
    let exponent = dp - 1;
    out.push('e');
    out.push(if exponent < 0 { '-' } else { '+' });
    let magnitude = exponent.unsigned_abs();
    let short = exponent_digits == ExponentDigits::ShortNegative && exponent < 0;
    if magnitude < 10 && !short {
        out.push('0');
    }
    let _ = write!(out, "{magnitude}");
}

/// `%f` with `max(len(digits) - dp, 0)` fraction digits.
fn push_f(out: &mut String, digits: &[u8], dp: i32) {
    let digit_at = |index: i32| -> char {
        usize::try_from(index)
            .ok()
            .and_then(|index| digits.get(index))
            .map_or('0', |&digit| char::from(digit))
    };
    if dp > 0 {
        for index in 0..dp {
            out.push(digit_at(index));
        }
    } else {
        out.push('0');
    }
    let fraction = i32::try_from(digits.len()).unwrap_or(i32::MAX) - dp;
    if fraction > 0 {
        out.push('.');
        for index in 0..fraction {
            out.push(digit_at(dp + index));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The S2 corpus of edge values, with Go's `FormatFloat(f, 'g', -1, 64)` output.
    #[test]
    fn format_g_matches_go_on_the_edge_values() {
        let cases = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "+Inf"),
            (f64::NEG_INFINITY, "-Inf"),
            (1e21, "1e+21"),
            (1e20, "1e+20"),
            (123_456.0, "123456"),
            (1_234_567.0, "1.234567e+06"),
            (1e6, "1e+06"),
            (999_999.0, "999999"),
            (100_000.0, "100000"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (1.729_771_234e9, "1.729771234e+09"),
            // 1729771234.123456789, as the nearest float64.
            (
                f64::from_bits(0x41d9_c68d_b887_e6b7),
                "1.7297712341234567e+09",
            ),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (2.5, "2.5"),
            (0.005, "0.005"),
            (-2.5e-7, "-2.5e-07"),
            (9_007_199_254_740_992.0, "9.007199254740992e+15"),
            // Exactly halfway between two 17-digit candidates: Go rounds to even, {:e} up.
            // 2171718000277121.25, which is a float64 exactly.
            (
                f64::from_bits(0x431e_dca9_ffd5_da05),
                "2.1717180002771212e+15",
            ),
        ];
        for (value, want) in cases {
            assert_eq!(format_g(value), want, "format_g({value:e})");
        }
    }

    /// encoding/json's layout, from the S4 corpus.
    #[test]
    fn format_json_matches_encoding_json() {
        let cases = [
            (0.5, "0.5"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (1e-6, "0.000001"),
            (123_456_789.0, "123456789"),
            (-0.0, "-0"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
        ];
        for (value, want) in cases {
            assert_eq!(
                format_json(value).as_deref(),
                Ok(want),
                "format_json({value:e})"
            );
        }
        assert_eq!(
            format_json(f64::NAN).unwrap_err().to_string(),
            "json: unsupported value: NaN"
        );
        assert_eq!(
            format_json(f64::NEG_INFINITY).unwrap_err().to_string(),
            "json: unsupported value: -Inf"
        );
    }
}
