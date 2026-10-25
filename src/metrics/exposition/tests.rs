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

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;

use flate2::read::GzDecoder;

use super::{Encoding, content_type, go_content_type, negotiate_encoding, parse_accept, render};
use crate::metrics::{Desc, Gauge, Kind, Registry};

/// One row of `go-negotiation.tsv`: the `Accept` and `Accept-Encoding` header values (each list
/// `|`-separated, `-` for no header), and the `Content-Type` and `Content-Encoding` promhttp
/// answered.
struct GoAnswer<'a> {
    accept: Vec<&'a str>,
    accept_encoding: Vec<&'a str>,
    content_type: &'a str,
    content_encoding: &'a str,
}

fn header_values(field: &str) -> Vec<&str> {
    if field == "-" {
        Vec::new()
    } else {
        field.split('|').collect()
    }
}

fn go_answers(data: &str) -> Vec<GoAnswer<'_>> {
    data.lines()
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            assert_eq!(columns.len(), 4, "malformed line {line:?}");
            GoAnswer {
                accept: header_values(columns[0]),
                accept_encoding: header_values(columns[1]),
                content_type: columns[2],
                content_encoding: columns[3],
            }
        })
        .collect()
}

/// promhttp's answers, the Prometheus Go client v1.24.1 and common v0.71.0, for 95 header pairs
/// from a throwaway Go program serving a `promhttp.HandlerFor` handler: the first 24 rows are S2's
/// `negotiation.tsv`, the rest probe the parsers (q values `ParseFloat` reads, more than 12
/// clauses, so goautoneg's sort leaves insertion sort, repeated headers, and the gzip rules). Each
/// row asserts Go's `Content-Type` for the negotiation itself, the Q2 text `Content-Type` with Go's
/// escaping where Go picks protobuf, and the `Content-Encoding`.
#[test]
fn negotiation_matches_go_on_95_header_pairs() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/exposition/go-negotiation.tsv");
    let data = fs::read_to_string(&path).expect("read go-negotiation.tsv");
    let answers = go_answers(&data);
    assert_eq!(answers.len(), 95);
    let mut protobuf = 0;
    for answer in &answers {
        // Go reads the first Accept header only (Header.Get).
        let accept = answer.accept.first().copied();
        let context = format!("{:?} {:?}", answer.accept, answer.accept_encoding);
        assert_eq!(go_content_type(accept), answer.content_type, "{context}");
        let (_, escaping) = answer
            .content_type
            .rsplit_once("; escaping=")
            .expect("Go always appends escaping");
        let want = format!("text/plain; version=0.0.4; charset=utf-8; escaping={escaping}");
        assert_eq!(content_type(accept), want, "{context}");
        if !answer.content_type.starts_with("text/plain") {
            protobuf += 1;
        }
        let want_encoding =
            (!answer.content_encoding.is_empty()).then_some(answer.content_encoding);
        assert_eq!(
            negotiate_encoding(&answer.accept_encoding).header(),
            want_encoding,
            "{context}"
        );
    }
    assert!(
        protobuf >= 10,
        "only {protobuf} rows where Go picks protobuf"
    );
}

/// goautoneg's parse: spaces trimmed around the parts, `q` read untrimmed, a malformed clause
/// skipped, and the last of a repeated parameter kept.
#[test]
fn parse_accept_follows_goautoneg() {
    let clauses = parse_accept(" text/plain ; a=1;a=2 ; q=0.5,*,bad,x/y/z,, application/json;q= 1");
    let summary: Vec<(String, String, f64, String)> = clauses
        .iter()
        .map(|clause| {
            (
                clause.media_type.clone(),
                clause.subtype.clone(),
                clause.quality,
                clause.param("a").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("*".to_owned(), "*".to_owned(), 1.0, String::new()),
            ("text".to_owned(), "plain".to_owned(), 0.5, "2".to_owned()),
            (
                "application".to_owned(),
                "json".to_owned(),
                0.0,
                String::new()
            ),
        ]
    );
}

/// The gzip rules, against the Go answers' reasons: a tie goes to identity, the name is
/// case-sensitive, a best q of 0 is identity, and every header counts.
#[test]
fn negotiate_encoding_rules() {
    assert_eq!(negotiate_encoding(&[]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip"]), Encoding::Gzip);
    assert_eq!(negotiate_encoding(&["gzip, identity"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["GZIP"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip;q=0"]), Encoding::Identity);
    assert_eq!(
        negotiate_encoding(&["identity;q=0, gzip;q=0"]),
        Encoding::Identity
    );
    assert_eq!(negotiate_encoding(&["deflate", "gzip"]), Encoding::Gzip);
    // gddo reads q as 0 or 1 then decimals: 2 is not a quality, so the element is dropped.
    assert_eq!(negotiate_encoding(&["gzip;q=2"]), Encoding::Identity);
    assert_eq!(negotiate_encoding(&["gzip;q=1.5"]), Encoding::Gzip);
}

fn one_gauge() -> Registry {
    let registry = Registry::new();
    let gauge = Arc::new(Gauge::new(
        Desc::new("g", "h", Kind::Gauge, &[]).expect("desc"),
    ));
    gauge.set(1.0);
    registry.register(gauge).expect("register");
    let other = Arc::new(Gauge::new(
        Desc::new("o", "other", Kind::Gauge, &[]).expect("desc"),
    ));
    registry.register(other).expect("register");
    registry
}

const ONE_GAUGE: &str = "# HELP g h\n# TYPE g gauge\ng 1\n# HELP o other\n# TYPE o gauge\no 0\n";

/// The answer: the body as text, gzipped with Go's header bytes when gzip is negotiated (no
/// name, mtime 0, the default level's XFL 0 and OS 255, as Go's `compress/gzip` writes them).
#[test]
fn render_writes_text_and_gzip() {
    let families = one_gauge().gather();
    let plain = render(&families, None, &[], None);
    assert_eq!(
        plain.content_type,
        "text/plain; version=0.0.4; charset=utf-8; escaping=underscores"
    );
    assert_eq!(plain.content_encoding, None);
    assert_eq!(plain.body, ONE_GAUGE.as_bytes());

    let gzipped = render(
        &families,
        Some("text/plain;escaping=allow-utf-8"),
        &["gzip"],
        None,
    );
    assert_eq!(
        gzipped.content_type,
        "text/plain; version=0.0.4; charset=utf-8; escaping=allow-utf-8"
    );
    assert_eq!(gzipped.content_encoding, Some("gzip"));
    assert_eq!(
        gzipped.body[..10],
        [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]
    );
    let mut decoded = String::new();
    GzDecoder::new(gzipped.body.as_slice())
        .read_to_string(&mut decoded)
        .expect("gunzip");
    assert_eq!(decoded, ONE_GAUGE);
}

/// promhttp's `name[]` filter keeps only the families named; an unknown name selects nothing.
#[test]
fn render_filters_by_name() {
    let families = one_gauge().gather();
    let names = BTreeSet::from(["o".to_owned(), "missing".to_owned()]);
    assert_eq!(
        render(&families, None, &[], Some(&names)).body,
        b"# HELP o other\n# TYPE o gauge\no 0\n"
    );
    let none = BTreeSet::from(["missing".to_owned()]);
    assert!(render(&families, None, &[], Some(&none)).body.is_empty());
}
