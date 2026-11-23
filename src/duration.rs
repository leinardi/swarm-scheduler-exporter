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

// Ported from humantime 2.4.0, src/duration.rs, MIT OR Apache-2.0, taken here under MIT:
// Copyright (c) 2016 The humantime Developers. Permission is hereby granted, free of charge, to
// any person obtaining a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the
// Software, and to permit persons to whom the Software is furnished to do so, subject to the
// following conditions: The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
// WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
// OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
// ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
// OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

//! The `--poll-delay` spellings and the way a delay prints: humantime 2.4.0's `parse_duration`
//! and `format_duration`, ported because humantime is unmaintained (RUSTSEC-2026-0542). What it
//! accepts, how it refuses and what it prints are Q17 in `docs/migration.md` and stay byte for
//! byte.
//!
//! A duration is one or more spans, each a whole or decimal number and a unit, with any
//! whitespace between them and none required (`1m30s`, `1m 30s`, `1.5s`); a lone `0` is zero.
//! The units, case-sensitive: `nanos`/`nsec`/`ns`, `usec`/`us`/`µs`, `millis`/`msec`/`ms`,
//! `seconds`/`second`/`secs`/`sec`/`s`, `minutes`/`minute`/`min`/`mins`/`m`,
//! `hours`/`hour`/`hr`/`hrs`/`h`, `days`/`day`/`d`, `weeks`/`week`/`wk`/`wks`/`w`,
//! `months`/`month`/`M` (30.44 days) and `years`/`year`/`yr`/`yrs`/`y` (365.25 days).

use std::fmt;
use std::str::Chars;
use std::time::Duration;

/// Why a text is not a duration; the texts are humantime's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// A character that belongs to no number or unit, at its byte offset.
    InvalidCharacter(usize),
    /// A span that does not start with a digit, at its byte offset.
    NumberExpected(usize),
    /// A number with no unit (`unit` empty) or a unit not in the list.
    UnknownUnit {
        /// The unit as written.
        unit: String,
        /// The number before it.
        value: u64,
    },
    /// A value that overflows, or a fraction finer than a nanosecond.
    NumberOverflow,
    /// No span at all.
    Empty,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCharacter(offset) => write!(formatter, "invalid character at {offset}"),
            Self::NumberExpected(offset) => write!(formatter, "expected number at {offset}"),
            Self::UnknownUnit { unit, value } if unit.is_empty() => write!(
                formatter,
                "time unit needed, for example {value}sec or {value}ms"
            ),
            Self::UnknownUnit { unit, .. } => write!(
                formatter,
                "unknown time unit {unit:?}, supported units: ns, us/µs, ms, sec, min, hours, \
                 days, weeks, months, years (and few variations)"
            ),
            Self::NumberOverflow => formatter.write_str(
                "number is too large or cannot be represented without a lack of precision \
                 (values below 1ns are not supported)",
            ),
            Self::Empty => formatter.write_str("value was empty"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Seconds in humantime's month, 30.44 days.
const MONTH_SECS: u64 = 2_630_016;
/// Seconds in humantime's year, 365.25 days.
const YEAR_SECS: u64 = 31_557_600;
const DAY_SECS: u64 = 86_400;
const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Parses a duration as humantime's `parse_duration` does.
///
/// # Errors
///
/// The first thing that is not part of a duration, as [`ParseError`] words it.
pub fn parse(text: &str) -> Result<Duration, ParseError> {
    if text == "0" {
        return Ok(Duration::ZERO);
    }
    Parser {
        iter: text.chars(),
        src: text,
    }
    .parse()
}

/// A duration as humantime's `format_duration` prints it: `0s`, else its nonzero parts from
/// years (`1year`, `2years`) through months and days to `h`, `m`, `s`, `ms`, `us` and `ns`,
/// separated by spaces (`1h 2m`, `1s 500ms`).
#[must_use]
pub fn format(duration: Duration) -> String {
    let secs = duration.as_secs();
    let nanos = u64::from(duration.subsec_nanos());
    if secs == 0 && nanos == 0 {
        return "0s".to_owned();
    }
    let in_year = secs % YEAR_SECS;
    let in_month = in_year % MONTH_SECS;
    let in_day = in_month % DAY_SECS;
    let parts: [(u64, &str, bool); 9] = [
        (secs / YEAR_SECS, "year", true),
        (in_year / MONTH_SECS, "month", true),
        (in_month / DAY_SECS, "day", true),
        (in_day / 3600, "h", false),
        (in_day % 3600 / 60, "m", false),
        (in_day % 60, "s", false),
        (nanos / 1_000_000, "ms", false),
        (nanos / 1000 % 1000, "us", false),
        (nanos % 1000, "ns", false),
    ];
    let mut out = String::new();
    for (value, name, plural) in parts {
        if value == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&value.to_string());
        out.push_str(name);
        if plural && value > 1 {
            out.push('s');
        }
    }
    out
}

fn mul(left: u64, right: u64) -> Result<u64, ParseError> {
    left.checked_mul(right).ok_or(ParseError::NumberOverflow)
}

fn add(left: u64, right: u64) -> Result<u64, ParseError> {
    left.checked_add(right).ok_or(ParseError::NumberOverflow)
}

/// An exact division, or overflow: a fraction that does not land on a whole nanosecond.
fn div(left: u64, right: u64) -> Result<u64, ParseError> {
    if left.is_multiple_of(right) {
        Ok(left / right)
    } else {
        Err(ParseError::NumberOverflow)
    }
}

/// `digit` as a number; only called on `'0'..='9'`.
fn digit(character: char) -> u64 {
    u64::from(character.to_digit(10).unwrap_or(0))
}

fn is_unit_char(character: char) -> bool {
    character.is_ascii_alphabetic() || character == 'µ'
}

#[derive(Debug, Clone, Copy)]
struct Fraction {
    numerator: u64,
    denominator: u64,
}

#[derive(Debug, Clone, Copy)]
enum Unit {
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "nanos" | "nsec" | "ns" => Self::Nanosecond,
            "usec" | "us" | "µs" => Self::Microsecond,
            "millis" | "msec" | "ms" => Self::Millisecond,
            "seconds" | "second" | "secs" | "sec" | "s" => Self::Second,
            "minutes" | "minute" | "min" | "mins" | "m" => Self::Minute,
            "hours" | "hour" | "hr" | "hrs" | "h" => Self::Hour,
            "days" | "day" | "d" => Self::Day,
            "weeks" | "week" | "wk" | "wks" | "w" => Self::Week,
            "months" | "month" | "M" => Self::Month,
            "years" | "year" | "yr" | "yrs" | "y" => Self::Year,
            _ => return None,
        })
    }

    /// `count` of the unit, as seconds and nanoseconds.
    fn whole(self, count: u64) -> Result<(u64, u64), ParseError> {
        Ok(match self {
            Self::Nanosecond => (0, count),
            Self::Microsecond => (0, mul(count, 1000)?),
            Self::Millisecond => (0, mul(count, 1_000_000)?),
            Self::Second => (count, 0),
            Self::Minute => (mul(count, 60)?, 0),
            Self::Hour => (mul(count, 3600)?, 0),
            Self::Day => (mul(count, DAY_SECS)?, 0),
            Self::Week => (mul(count, DAY_SECS * 7)?, 0),
            Self::Month => (mul(count, MONTH_SECS)?, 0),
            Self::Year => (mul(count, YEAR_SECS)?, 0),
        })
    }

    /// The fraction `numerator / denominator` of the unit, as seconds and nanoseconds; exact,
    /// or overflow. Below a minute it lands on nanoseconds, from an hour up on seconds.
    fn fraction(self, numerator: u64, denominator: u64) -> Result<(u64, u64), ParseError> {
        let nanos = |per: u64| Ok((0, div(mul(numerator, per)?, denominator)?));
        let secs = |per: u64| Ok((div(mul(numerator, per)?, denominator)?, 0));
        match self {
            Self::Nanosecond => Err(ParseError::NumberOverflow),
            Self::Microsecond => nanos(1000),
            Self::Millisecond => nanos(1_000_000),
            Self::Second => nanos(NANOS_PER_SEC),
            Self::Minute => nanos(60 * NANOS_PER_SEC),
            Self::Hour => secs(3600),
            Self::Day => secs(DAY_SECS),
            Self::Week => secs(DAY_SECS * 7),
            Self::Month => secs(MONTH_SECS),
            Self::Year => secs(YEAR_SECS),
        }
    }
}

/// Adds `secs` and `nanos` to `out`. Like humantime, it carries the nanoseconds into seconds
/// only above a whole second, so exactly 1e9 nanoseconds go into the `Duration` constructor,
/// which normalises them.
fn add_to(secs: u64, nanos: u64, out: &mut Duration) -> Result<(), ParseError> {
    let mut secs = secs;
    let mut nanos = add(u64::from(out.subsec_nanos()), nanos)?;
    if nanos > NANOS_PER_SEC {
        secs = add(secs, nanos / NANOS_PER_SEC)?;
        nanos %= NANOS_PER_SEC;
    }
    let secs = add(out.as_secs(), secs)?;
    let nanos = u32::try_from(nanos).map_err(|_| ParseError::NumberOverflow)?;
    *out = Duration::from_secs(secs)
        .checked_add(Duration::from_nanos(u64::from(nanos)))
        .ok_or(ParseError::NumberOverflow)?;
    Ok(())
}

struct Parser<'text> {
    iter: Chars<'text>,
    src: &'text str,
}

impl Parser<'_> {
    fn parse(mut self) -> Result<Duration, ParseError> {
        let mut count = self.first_digit()?.ok_or(ParseError::Empty)?;
        let mut out = Duration::ZERO;
        'span: loop {
            let mut fraction = None;
            let mut offset = self.offset();
            // The number, until a unit starts or a decimal point leads to the fraction.
            while let Some(character) = self.iter.next() {
                match character {
                    '0'..='9' => count = add(mul(count, 10)?, digit(character))?,
                    character if character.is_whitespace() => {}
                    character if is_unit_char(character) => break,
                    '.' => {
                        fraction = Some(self.fraction(&mut offset)?);
                        break;
                    }
                    _ => return Err(ParseError::InvalidCharacter(offset)),
                }
                offset = self.offset();
            }
            // The unit, until whitespace or the next span's first digit.
            let start = offset;
            let mut end = self.offset();
            while let Some(character) = self.iter.next() {
                match character {
                    '0'..='9' => {
                        self.add_span(count, fraction, start, end, &mut out)?;
                        count = digit(character);
                        continue 'span;
                    }
                    character if character.is_whitespace() => break,
                    character if is_unit_char(character) => {}
                    _ => return Err(ParseError::InvalidCharacter(end)),
                }
                end = self.offset();
            }
            self.add_span(count, fraction, start, end, &mut out)?;
            count = match self.first_digit()? {
                Some(count) => count,
                None => return Ok(out),
            };
        }
    }

    /// Skips whitespace to the next span's first digit; `None` at the end of the text.
    fn first_digit(&mut self) -> Result<Option<u64>, ParseError> {
        let offset = self.offset();
        for character in self.iter.by_ref() {
            match character {
                '0'..='9' => return Ok(Some(digit(character))),
                character if character.is_whitespace() => {}
                _ => return Err(ParseError::NumberExpected(offset)),
            }
        }
        Ok(None)
    }

    /// The digits after a decimal point, up to the unit. A point with no digit is an invalid
    /// character.
    fn fraction(&mut self, offset: &mut usize) -> Result<Fraction, ParseError> {
        let mut numerator = 0u64;
        let mut denominator = 1u64;
        let mut zeros = true;
        while let Some(character) = self.iter.next() {
            match character {
                '0' => {
                    denominator = mul(denominator, 10)?;
                    if !zeros {
                        numerator = mul(numerator, 10)?;
                    }
                }
                '1'..='9' => {
                    zeros = false;
                    denominator = mul(denominator, 10)?;
                    numerator = add(mul(numerator, 10)?, digit(character))?;
                }
                character if character.is_whitespace() => {}
                character if is_unit_char(character) => break,
                _ => return Err(ParseError::InvalidCharacter(*offset)),
            }
            *offset = self.offset();
        }
        if denominator == 1 {
            return Err(ParseError::InvalidCharacter(*offset));
        }
        Ok(Fraction {
            numerator,
            denominator,
        })
    }

    fn offset(&self) -> usize {
        self.src.len() - self.iter.as_str().len()
    }

    /// Adds `count` (and `fraction`) of the unit written at `start..end` to `out`.
    fn add_span(
        &self,
        count: u64,
        fraction: Option<Fraction>,
        start: usize,
        end: usize,
        out: &mut Duration,
    ) -> Result<(), ParseError> {
        let written = self.src.get(start..end).unwrap_or_default();
        let unit = Unit::from_text(written).ok_or_else(|| ParseError::UnknownUnit {
            unit: written.to_owned(),
            value: count,
        })?;
        let (secs, nanos) = unit.whole(count)?;
        add_to(secs, nanos, out)?;
        if let Some(Fraction {
            numerator,
            denominator,
        }) = fraction
        {
            let (secs, nanos) = unit.fraction(numerator, denominator)?;
            add_to(secs, nanos, out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
