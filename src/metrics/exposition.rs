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

//! The `/metrics` answer: promhttp's content negotiation, and the body.
//!
//! promhttp (client_golang v1.24.1, common v0.71.0) picks a format with expfmt's `Negotiate`
//! among Go's four (protobuf delimited, text and compact-text, and text 0.0.4) from the first
//! `Accept` header, which goautoneg parses and sorts, and appends `; escaping=` from the
//! `escaping` parameters it meets on the way. It picks a compression with gddo's
//! `NegotiateContentEncoding` over every `Accept-Encoding` header, offering identity then gzip.
//!
//! Q2: the exporter only writes text 0.0.4. The negotiation is ported in full anyway, protobuf
//! included, because it decides the `escaping` parameter: Go stops at the first clause that
//! matches any of its formats, so a protobuf clause ahead of a text one keeps the default
//! `underscores`. The `Content-Type` is then always the text one, with the escaping Go would have
//! echoed; the body is the same for every escaping, because every name the exporter writes is
//! legacy-valid.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::gofmt_compat::float;
use crate::gofmt_compat::parse_float::{ParseFloatError, parse_float32};
use crate::gofmt_compat::sort;

use super::Gathered;
use super::text::{self, TEXT_CONTENT_TYPE};

/// One clause of an `Accept` header, as goautoneg's `Accept`.
#[derive(Debug, Clone, PartialEq)]
struct AcceptClause {
    media_type: String,
    subtype: String,
    quality: f64,
    // A Go map: a parameter given twice keeps its last value.
    params: BTreeMap<String, String>,
}

impl AcceptClause {
    fn param(&self, key: &str) -> &str {
        self.params.get(key).map_or("", String::as_str)
    }
}

/// goautoneg's `nextSplitElement`: before and after the first `separator`.
fn next_split(text: &str, separator: char) -> (&str, &str) {
    text.split_once(separator).unwrap_or((text, ""))
}

/// goautoneg's trim, of spaces only.
fn trim_spaces(text: &str) -> &str {
    text.trim_matches(' ')
}

/// goautoneg's `ParseAccept`: the clauses, sorted by Go's `sort.Sort` with goautoneg's
/// comparison (higher q first, then a concrete type before `*`, then a concrete subtype before
/// `*`), which is not a total order, so only Go's algorithm gives Go's order.
fn parse_accept(header: &str) -> Vec<AcceptClause> {
    let mut clauses = Vec::new();
    let mut remaining = header;
    while !remaining.is_empty() {
        let (part, rest) = next_split(remaining, ',');
        remaining = rest;
        let part = trim_spaces(part);
        let (media_range, mut params) = next_split(part, ';');
        let (media_type, subtype_rest) = next_split(media_range, '/');
        let media_type = trim_spaces(media_type).to_owned();
        let subtype = if subtype_rest.is_empty() {
            if media_type != "*" {
                continue;
            }
            "*".to_owned()
        } else {
            let (subtype, extra) = next_split(subtype_rest, '/');
            if !extra.is_empty() {
                continue;
            }
            trim_spaces(subtype).to_owned()
        };
        let mut clause = AcceptClause {
            media_type,
            subtype,
            quality: 1.0,
            params: BTreeMap::new(),
        };
        while !params.is_empty() {
            let (param, rest) = next_split(params, ';');
            params = rest;
            let (key, value_rest) = next_split(param, '=');
            if value_rest.is_empty() {
                continue;
            }
            let (value, extra) = next_split(value_rest, '=');
            if !extra.is_empty() {
                continue;
            }
            let token = trim_spaces(key);
            if token == "q" {
                // a.Q, _ = strconv.ParseFloat(value, 32): untrimmed, 0 on a syntax error, the
                // infinity on a range error.
                clause.quality = f64::from(match parse_float32(value) {
                    Ok(quality) | Err(ParseFloatError::Range(quality)) => quality,
                    Err(ParseFloatError::Syntax) => 0.0,
                });
            } else {
                clause
                    .params
                    .insert(token.to_owned(), trim_spaces(value).to_owned());
            }
        }
        clauses.push(clause);
    }
    sort::sort(&mut clauses, |left, right| {
        left.quality > right.quality
            || (left.media_type != "*" && right.media_type == "*")
            || (left.subtype != "*" && right.subtype == "*")
    });
    clauses
}

/// One of the formats Go's `Negotiate` offers, as its `Accept` clause would parse.
struct GoFormat {
    content_type: &'static str,
    media_type: &'static str,
    subtype: &'static str,
    params: &'static [(&'static str, &'static str)],
}

const PROTOBUF: &str = "vnd.google.protobuf";
const METRIC_FAMILY: &str = "io.prometheus.client.MetricFamily";

/// `Negotiate`'s offers, in its order: `FmtProtoDelim`, `FmtProtoText`, `FmtProtoCompact`,
/// `FmtText`.
const GO_FORMATS: [GoFormat; 4] = [
    GoFormat {
        content_type: "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; \
                       encoding=delimited",
        media_type: "application",
        subtype: PROTOBUF,
        params: &[("proto", METRIC_FAMILY), ("encoding", "delimited")],
    },
    GoFormat {
        content_type: "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; \
                       encoding=text",
        media_type: "application",
        subtype: PROTOBUF,
        params: &[("proto", METRIC_FAMILY), ("encoding", "text")],
    },
    GoFormat {
        content_type: "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; \
                       encoding=compact-text",
        media_type: "application",
        subtype: PROTOBUF,
        params: &[("proto", METRIC_FAMILY), ("encoding", "compact-text")],
    },
    GoFormat {
        content_type: TEXT_CONTENT_TYPE,
        media_type: "text",
        subtype: "plain",
        params: &[("version", "0.0.4"), ("charset", "utf-8")],
    },
];

/// The index of the text format in [`GO_FORMATS`].
const GO_TEXT: usize = 3;

/// expfmt's `matchFormat`: the type and subtype match or are `*`, and every parameter of the
/// format but `charset` is equal, the version defaulting to 0.0.1 for OpenMetrics and 0.0.4 for
/// text.
fn matches_format(clause: &AcceptClause, format: &GoFormat) -> bool {
    if clause.media_type != "*" && clause.media_type != format.media_type {
        return false;
    }
    if clause.subtype != "*" && clause.subtype != format.subtype {
        return false;
    }
    let mut version = clause.param("version");
    if version.is_empty()
        && clause.media_type == "application"
        && clause.subtype == "openmetrics-text"
    {
        version = "0.0.1";
    }
    if version.is_empty() && clause.media_type == "text" && clause.subtype == "plain" {
        version = "0.0.4";
    }
    format.params.iter().all(|&(key, want)| {
        key == "charset"
            || (if key == "version" {
                version
            } else {
                clause.param(key)
            }) == want
    })
}

/// What `NegotiateAccept` picks for an `Accept` header: the format among [`GO_FORMATS`], and the
/// escaping scheme.
fn negotiate_accept(accept: &str) -> (usize, String) {
    let mut escaping = "underscores".to_owned();
    for clause in parse_accept(accept) {
        let escape_param = clause.param("escaping");
        // An unknown scheme is ignored.
        if matches!(
            escape_param,
            "allow-utf-8" | "underscores" | "dots" | "values"
        ) {
            escape_param.clone_into(&mut escaping);
        }
        if let Some(format) = GO_FORMATS
            .iter()
            .position(|format| matches_format(&clause, format))
        {
            return (format, escaping);
        }
    }
    (GO_TEXT, escaping)
}

/// The `Content-Type` Go's promhttp answers for the first `Accept` header (`None` without one).
#[must_use]
pub fn go_content_type(accept: Option<&str>) -> String {
    let (format, escaping) = negotiate_accept(accept.unwrap_or_default());
    format!("{}; escaping={escaping}", GO_FORMATS[format].content_type)
}

/// The `Content-Type` the exporter answers (Q2): always text 0.0.4, with the escaping Go would
/// have echoed for the first `Accept` header.
#[must_use]
pub fn content_type(accept: Option<&str>) -> String {
    let (_, escaping) = negotiate_accept(accept.unwrap_or_default());
    format!("{TEXT_CONTENT_TYPE}; escaping={escaping}")
}

/// A compression promhttp offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// No compression, and no `Content-Encoding`.
    Identity,
    /// `Content-Encoding: gzip`.
    Gzip,
}

impl Encoding {
    /// The `Content-Encoding` value, or none for identity.
    #[must_use]
    pub fn header(self) -> Option<&'static str> {
        match self {
            Self::Identity => None,
            Self::Gzip => Some("gzip"),
        }
    }
}

/// One `Accept-Encoding` element, as gddo's `AcceptSpec`.
struct AcceptSpec<'a> {
    value: &'a str,
    quality: f64,
}

/// gddo's `octetTypes`: a token character.
fn is_token(byte: u8) -> bool {
    byte.is_ascii() && !byte.is_ascii_control() && !b" \t\"(),/:;<=>?@[]\\{}".contains(&byte)
}

/// gddo's `isSpace`: space, tab, CR or LF.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_space(text: &str) -> &str {
    text.trim_start_matches(|character: char| u8::try_from(character).is_ok_and(is_space))
}

/// gddo's `expectTokenSlash`: the leading run of token characters and `/`.
fn expect_token_slash(text: &str) -> (&str, &str) {
    let end = text
        .bytes()
        .position(|byte| !is_token(byte) && byte != b'/')
        .unwrap_or(text.len());
    text.split_at(end)
}

/// gddo's `expectQuality`: `0` or `1`, then optionally a point and digits; -1 when it starts
/// otherwise. The digits accumulate in Go `int`s, wrapping as they do.
fn expect_quality(text: &str) -> (f64, &str) {
    let quality = match text.as_bytes().first() {
        Some(b'0') => 0.0,
        Some(b'1') => 1.0,
        _ => return (-1.0, ""),
    };
    let rest = &text[1..];
    let Some(rest) = rest.strip_prefix('.') else {
        return (quality, rest);
    };
    let end = rest
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(rest.len());
    let (mut numerator, mut denominator) = (0_i64, 1_i64);
    for byte in rest[..end].bytes() {
        numerator = numerator
            .wrapping_mul(10)
            .wrapping_add(i64::from(byte - b'0'));
        denominator = denominator.wrapping_mul(10);
    }
    (
        quality + float::from_int(numerator) / float::from_int(denominator),
        &rest[end..],
    )
}

/// gddo's `header.ParseAccept` over every value of the header.
fn parse_accept_encoding<'a>(values: &[&'a str]) -> Vec<AcceptSpec<'a>> {
    let mut specs = Vec::new();
    'values: for &header in values {
        let mut text = header;
        loop {
            let (value, rest) = expect_token_slash(text);
            if value.is_empty() {
                continue 'values;
            }
            let mut quality = 1.0;
            text = skip_space(rest);
            if let Some(after) = text.strip_prefix(';') {
                let Some(after) = skip_space(after).strip_prefix("q=") else {
                    continue 'values;
                };
                let rest;
                (quality, rest) = expect_quality(after);
                text = rest;
                if quality < 0.0 {
                    continue 'values;
                }
            }
            specs.push(AcceptSpec { value, quality });
            text = skip_space(text);
            let Some(after) = text.strip_prefix(',') else {
                continue 'values;
            };
            text = skip_space(after);
        }
    }
    specs
}

/// gddo's `NegotiateContentEncoding` over promhttp's offers, identity then gzip: the first offer
/// with a strictly higher q wins, so a tie goes to identity, and a best q of 0 (which gddo turns
/// into no encoding) is identity too. Values are compared case-sensitively.
#[must_use]
pub fn negotiate_encoding(accept_encoding: &[&str]) -> Encoding {
    let specs = parse_accept_encoding(accept_encoding);
    let mut best = Encoding::Identity;
    let mut best_q = -1.0;
    for (offer, name) in [(Encoding::Identity, "identity"), (Encoding::Gzip, "gzip")] {
        for spec in &specs {
            if spec.quality > best_q && (spec.value == "*" || spec.value == name) {
                best_q = spec.quality;
                best = offer;
            }
        }
    }
    if best_q == 0.0 {
        return Encoding::Identity;
    }
    best
}

/// A `/metrics` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposition {
    /// The `Content-Type`.
    pub content_type: String,
    /// The `Content-Encoding`, if compressed.
    pub content_encoding: Option<&'static str>,
    /// The body, compressed if `content_encoding` says so.
    pub body: Vec<u8>,
}

/// The answer for `families`, given the first `Accept` header and every `Accept-Encoding` one.
/// `names` is promhttp's `name[]` query filter: when set, only those families are written.
#[must_use]
pub fn render(
    families: &[Gathered],
    accept: Option<&str>,
    accept_encoding: &[&str],
    names: Option<&BTreeSet<String>>,
) -> Exposition {
    let mut body = String::new();
    let selected: Vec<Gathered> = families
        .iter()
        .filter(|family| names.is_none_or(|names| names.contains(&family.name)))
        .cloned()
        .collect();
    text::encode(&mut body, &selected);
    let encoding = negotiate_encoding(accept_encoding);
    let body = match encoding {
        Encoding::Identity => body.into_bytes(),
        Encoding::Gzip => gzip(body.as_bytes()),
    };
    Exposition {
        content_type: content_type(accept),
        content_encoding: encoding.header(),
        body,
    }
}

/// gzip at the default level, with Go's header: no name, mtime 0 and OS 255 (unknown).
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    // Writing to and finishing into a Vec cannot fail.
    if encoder.write_all(data).is_err() {
        return Vec::new();
    }
    encoder.finish().unwrap_or_default()
}

#[cfg(test)]
mod tests;
