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

//! Go's two RFC 3339 layouts: `time.RFC3339Nano` (`2006-01-02T15:04:05.999999999Z07:00`) and
//! slog's text time (`2006-01-02T15:04:05.000Z07:00`).
//!
//! jiff's own formats are not Go's (S7): they write UTC as `+00:00` rather than `Z`, and round a
//! sub-minute offset (a local mean time such as Amsterdam's `+00:19:32` in 1900) where Go
//! truncates it to `+00:19`. The zone is written exactly as Go's `appendFormatRFC3339` does.

use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::tz::Offset;

const NANOS_PER_MILLI: i32 = 1_000_000;

/// The instant Go's zero `time.Time` stands for, `0001-01-01T00:00:00Z`; `IsZero` compares the
/// instant only, whatever the zone.
#[must_use]
pub fn go_zero_time() -> Timestamp {
    Timestamp::new(-62_135_596_800, 0).unwrap_or(Timestamp::MIN)
}

/// Go's `Time.IsZero`.
#[must_use]
pub fn is_go_zero_time(timestamp: Timestamp) -> bool {
    timestamp == go_zero_time()
}

/// Appends `timestamp` at `offset` in `time.RFC3339Nano`: the fraction with its trailing zeros
/// trimmed, and none when it is zero.
pub fn push_rfc3339_nano(out: &mut String, timestamp: Timestamp, offset: Offset) {
    let nanos = push_date_time(out, timestamp, offset);
    if nanos != 0 {
        let fraction = format!("{nanos:09}");
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    push_zone(out, offset);
}

/// Appends `timestamp` at `offset` in `2006-01-02T15:04:05.000Z07:00`, the fraction truncated to
/// the millisecond.
pub fn push_rfc3339_millis(out: &mut String, timestamp: Timestamp, offset: Offset) {
    let nanos = push_date_time(out, timestamp, offset);
    let _ = write!(out, ".{:03}", nanos / NANOS_PER_MILLI);
    push_zone(out, offset);
}

/// `timestamp` at `offset` in `time.RFC3339Nano`.
#[must_use]
pub fn format_rfc3339_nano(timestamp: Timestamp, offset: Offset) -> String {
    let mut out = String::with_capacity(35);
    push_rfc3339_nano(&mut out, timestamp, offset);
    out
}

/// Writes the civil date and time, to the second, and returns the nanoseconds of the second.
fn push_date_time(out: &mut String, timestamp: Timestamp, offset: Offset) -> i32 {
    let civil = offset.to_datetime(timestamp);
    let year = i32::from(civil.year());
    // Go's appendInt(year, 4): a sign, then at least four digits.
    if year < 0 {
        out.push('-');
    }
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        year.unsigned_abs(),
        civil.month(),
        civil.day(),
        civil.hour(),
        civil.minute(),
        civil.second()
    );
    civil.subsec_nanosecond()
}

/// Go's `Z07:00`: `Z` for a zero offset, otherwise the sign and the hours and minutes, with the
/// seconds of the offset dropped (the minutes are the offset divided by 60, truncated toward
/// zero, so an offset of -30 seconds writes `+00:00`).
fn push_zone(out: &mut String, offset: Offset) {
    let seconds = offset.seconds();
    if seconds == 0 {
        out.push('Z');
        return;
    }
    let mut minutes = seconds / 60;
    if minutes < 0 {
        out.push('-');
        minutes = -minutes;
    } else {
        out.push('+');
    }
    let _ = write!(out, "{:02}:{:02}", minutes / 60, minutes % 60);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset(seconds: i32) -> Offset {
        Offset::from_seconds(seconds).unwrap()
    }

    fn at(seconds: i64, nanos: i32) -> Timestamp {
        Timestamp::new(seconds, nanos).unwrap()
    }

    fn millis(timestamp: Timestamp, offset: Offset) -> String {
        let mut out = String::new();
        push_rfc3339_millis(&mut out, timestamp, offset);
        out
    }

    /// Go's output for the S4 record time, 2026-10-25T14:03:04.567891234+01:00.
    #[test]
    fn layouts_match_go_on_the_record_time() {
        let record = at(1_792_933_384, 567_891_234);
        assert_eq!(
            format_rfc3339_nano(record, offset(3600)),
            "2026-10-25T14:03:04.567891234+01:00"
        );
        assert_eq!(
            millis(record, offset(3600)),
            "2026-10-25T14:03:04.567+01:00"
        );
    }

    /// The zone and fraction rules of the S7 corpus.
    #[test]
    fn layouts_follow_go_zone_and_fraction_rules() {
        let utc = at(1_767_323_045, 120_000_000); // 2026-01-02T03:04:05.12Z
        assert_eq!(
            format_rfc3339_nano(utc, Offset::UTC),
            "2026-01-02T03:04:05.12Z"
        );
        assert_eq!(millis(utc, Offset::UTC), "2026-01-02T03:04:05.120Z");
        let whole = at(1_767_323_045, 0);
        assert_eq!(
            format_rfc3339_nano(whole, Offset::UTC),
            "2026-01-02T03:04:05Z"
        );
        // 999.5ms truncates to .999, never rounds up.
        assert_eq!(
            millis(at(1_767_323_045, 999_500_000), Offset::UTC),
            "2026-01-02T03:04:05.999Z"
        );
        // -05:30, and a local mean time offset whose seconds Go drops.
        assert_eq!(
            format_rfc3339_nano(at(946_704_599, 999_999_999), offset(-(5 * 3600 + 30 * 60))),
            "1999-12-31T23:59:59.999999999-05:30"
        );
        assert_eq!(
            format_rfc3339_nano(at(-2_208_988_800, 0), offset(1172)),
            "1900-01-01T00:19:32+00:19"
        );
        assert_eq!(
            format_rfc3339_nano(at(0, 0), offset(-30)),
            "1969-12-31T23:59:30+00:00"
        );
    }

    #[test]
    fn go_zero_time_formats_as_year_one() {
        assert!(is_go_zero_time(go_zero_time()));
        assert_eq!(
            format_rfc3339_nano(go_zero_time(), Offset::UTC),
            "0001-01-01T00:00:00Z"
        );
        assert_eq!(
            millis(go_zero_time(), Offset::UTC),
            "0001-01-01T00:00:00.000Z"
        );
    }
}
