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

//! Go's `time.Duration.String`: `1h2m3.5s`, `1.5µs`, `0s`. A duration is an `i64` of
//! nanoseconds, as Go's is; the whole range prints, `-2562047h47m16.854775808s` included.

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
}
