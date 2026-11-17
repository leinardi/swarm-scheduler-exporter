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

//! humantime 2.4.0's own cases for `parse_duration` and `format_duration`, which the port must
//! keep, and a round trip over a deterministic spread of durations in place of its random ones.

use std::time::Duration;

use super::{ParseError, format, parse};

fn ok(text: &str, secs: u64, nanos: u32) {
    assert_eq!(parse(text), Ok(Duration::new(secs, nanos)), "{text:?}");
}

#[test]
fn every_unit_spelling() {
    for (text, secs, nanos) in [
        ("17nsec", 0, 17),
        ("17nanos", 0, 17),
        ("33ns", 0, 33),
        ("3usec", 0, 3000),
        ("78us", 0, 78_000),
        ("163µs", 0, 163_000),
        ("31msec", 0, 31_000_000),
        ("31millis", 0, 31_000_000),
        ("6ms", 0, 6_000_000),
        ("3000s", 3000, 0),
        ("300sec", 300, 0),
        ("300secs", 300, 0),
        ("50seconds", 50, 0),
        ("1second", 1, 0),
        ("100m", 6000, 0),
        ("12min", 720, 0),
        ("12mins", 720, 0),
        ("1minute", 60, 0),
        ("7minutes", 420, 0),
        ("2h", 7200, 0),
        ("7hr", 25_200, 0),
        ("7hrs", 25_200, 0),
        ("1hour", 3600, 0),
        ("24hours", 86_400, 0),
        ("1day", 86_400, 0),
        ("2days", 172_800, 0),
        ("365d", 31_536_000, 0),
        ("1week", 604_800, 0),
        ("7weeks", 4_233_600, 0),
        ("104wks", 2 * 31_449_600, 0),
        ("100wk", 60_480_000, 0),
        ("52w", 31_449_600, 0),
        ("1month", 2_630_016, 0),
        ("3months", 3 * 2_630_016, 0),
        ("12M", 31_560_192, 0),
        ("1year", 31_557_600, 0),
        ("7years", 7 * 31_557_600, 0),
        ("15yrs", 15 * 31_557_600, 0),
        ("10yr", 10 * 31_557_600, 0),
        ("17y", 536_479_200, 0),
    ] {
        ok(text, secs, nanos);
    }
}

#[test]
fn fractions() {
    for (text, secs, nanos) in [
        ("3.1usec", 0, 3100),
        ("3.01us", 0, 3010),
        ("3.001us", 0, 3001),
        ("31.1ms", 0, 31_100_000),
        ("31.000001ms", 0, 31_000_001),
        ("300.0seconds", 300, 0),
        ("0.0s", 0, 0),
        ("0.2s", 0, 200_000_000),
        ("1.02s", 1, 20_000_000),
        ("1.000000002s", 1, 2),
        ("1.123456789s", 1, 123_456_789),
        ("12.1min", 726, 0),
        ("1.5minutes", 90, 0),
        ("2.5h", 9000, 0),
        ("0.5h", 1800, 0),
        ("1.5d", 129_600, 0),
        ("7.120us 3ns", 0, 7123),
        ("1.234s 0.789us", 1, 234_000_789),
        ("1.234s 1.345ms 1.678us 1ns", 1, 235_346_679),
        ("1.234s0.345ms0.678us0ns", 1, 234_345_678),
    ] {
        ok(text, secs, nanos);
    }
    for text in [
        "17.5nsec",
        "0.0005ns",
        "3.0001us",
        "31.0000001ms",
        "1.0000000002s",
        "0.000123456789s",
        "0.00000005d",
    ] {
        assert_eq!(parse(text), Err(ParseError::NumberOverflow), "{text:?}");
    }
    for text in ["1.s", "1..s"] {
        assert!(
            matches!(parse(text), Err(ParseError::InvalidCharacter(_))),
            "{text:?}"
        );
    }
    for text in [".1s", "."] {
        assert!(
            matches!(parse(text), Err(ParseError::NumberExpected(_))),
            "{text:?}"
        );
    }
}

#[test]
fn spans_combine_with_or_without_spaces() {
    ok("0", 0, 0);
    ok("20 min 17 nsec ", 1200, 17);
    ok("2h 15m", 8100, 0);
    ok("1m30s", 90, 0);
}

#[test]
fn overflow() {
    for text in [
        "100000000000000000000ns",
        "100000000000000000us",
        "100000000000000ms",
        "100000000000000000000s",
        "10000000000000000000m",
        "1000000000000000000h",
        "100000000000000000d",
        "10000000000000000w",
        "1000000000000000M",
        "10000000000000y",
    ] {
        assert_eq!(parse(text), Err(ParseError::NumberOverflow), "{text:?}");
    }
}

#[test]
fn the_error_texts() {
    for (text, message) in [
        ("123", "time unit needed, for example 123sec or 123ms"),
        ("10 months 1", "time unit needed, for example 1sec or 1ms"),
        (
            "10nights",
            "unknown time unit \"nights\", supported units: ns, us/µs, ms, sec, min, hours, \
             days, weeks, months, years (and few variations)",
        ),
        ("\0", "expected number at 0"),
        ("-5s", "expected number at 0"),
        ("\r", "value was empty"),
        ("", "value was empty"),
        ("1~", "invalid character at 1"),
        ("1Nå", "invalid character at 2"),
        (
            "222nsec221nanosmsec7s5msec572s",
            "unknown time unit \"nanosmsec\", supported units: ns, us/µs, ms, sec, min, hours, \
             days, weeks, months, years (and few variations)",
        ),
        (
            "1000000000000000M",
            "number is too large or cannot be represented without a lack of precision (values \
             below 1ns are not supported)",
        ),
    ] {
        assert_eq!(parse(text).unwrap_err().to_string(), message, "{text:?}");
    }
}

#[test]
fn the_printed_forms() {
    for (duration, text) in [
        (Duration::ZERO, "0s"),
        (Duration::from_secs(10), "10s"),
        (Duration::from_mins(62), "1h 2m"),
        (Duration::from_millis(1500), "1s 500ms"),
        (Duration::new(9420, 0), "2h 37m"),
        (Duration::new(0, 32_000_000), "32ms"),
        (Duration::from_micros(123), "123us"),
        (Duration::from_hours(24), "1day"),
        (Duration::from_secs(2 * 86_400 + 1), "2days 1s"),
        (
            Duration::new(31_557_600 + 2_630_016 * 2, 7),
            "1year 2months 7ns",
        ),
    ] {
        assert_eq!(format(duration), text, "{duration:?}");
    }
}

/// Every printed form reads back as the same duration: each second of a day, as humantime
/// checks, and a deterministic spread up to year 9999 with nanoseconds, in place of its random
/// samples.
#[test]
fn a_printed_duration_parses_back() {
    for second in 0..86_400 {
        let duration = Duration::from_secs(second);
        assert_eq!(parse(&format(duration)), Ok(duration), "{duration:?}");
    }
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..10_000 {
        // xorshift64: no random source needed for a spread.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let duration = Duration::new(
            state % 253_370_764_800,
            u32::try_from(state % 1_000_000_000).unwrap(),
        );
        assert_eq!(parse(&format(duration)), Ok(duration), "{duration:?}");
    }
}
