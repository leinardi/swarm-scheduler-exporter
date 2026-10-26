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

//! The logger against the S4 capture: the same 54 records, logged by Go v1.26 through slog's
//! text and JSON handlers and the exporter's plain handler, each with and without the time
//! (`tests/testdata/logger/{text,json,plain}_{time,notime}.log`), compared line for line.
//!
//! Every record is logged at 2026-10-25T14:03:04.567891234+01:00. Three of the 54 hold invalid
//! UTF-8 (`"a\xffb"`, `"\xff"` and `[]string{"\xff"}`), which a Rust string cannot carry, so
//! 51 records per file, 306 in all, are compared.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use swarm_scheduler_exporter::go_compat::time_layout;
use swarm_scheduler_exporter::logger::{Attr, Config, Format, Level, Logger, MemorySink};

/// The record time, 2026-10-25T14:03:04.567891234+01:00.
fn record_time() -> Timestamp {
    Timestamp::new(1_792_933_384, 567_891_234).unwrap_or_else(|error| panic!("{error}"))
}

fn cet() -> Offset {
    Offset::from_seconds(3600).unwrap_or_else(|error| panic!("{error}"))
}

fn at(seconds: i64, nanos: i32) -> Timestamp {
    Timestamp::new(seconds, nanos).unwrap_or_else(|error| panic!("{error}"))
}

fn offset(seconds: i32) -> Offset {
    Offset::from_seconds(seconds).unwrap_or_else(|error| panic!("{error}"))
}

/// How a case derives its logger from the base one.
type With = fn(&Logger) -> Logger;

/// One corpus record; `None` stands for a record Go logged with invalid UTF-8.
struct Case {
    level: Level,
    message: String,
    attrs: Vec<Attr>,
    with: Option<With>,
}

fn case(level: Level, message: &str, attrs: Vec<Attr>) -> Case {
    Case {
        level,
        message: message.to_owned(),
        attrs,
        with: None,
    }
}

fn with_case(message: &str, attrs: Vec<Attr>, with: With) -> Case {
    Case {
        level: Level::INFO,
        message: message.to_owned(),
        attrs,
        with: Some(with),
    }
}

/// The S4 corpus, in the Go program's order.
fn corpus() -> Vec<Option<Case>> {
    let strings: [Option<&str>; 31] = [
        Some("plain"),
        Some("with space"),
        Some(r#"say "hi""#),
        Some("k=v"),
        Some(r"C:\path\x"),
        Some("line1\nline2"),
        Some("a\tb"),
        Some("a\rb"),
        Some("a\x01b"),
        Some("a\x7fb"),
        Some("a\x08b\x0cc"),
        None, // "a\xffb"
        None, // "\xff"
        Some("ok 🚀"),
        Some("🚀"),
        Some(""),
        Some("html <a>&amp;"),
        Some("nbsp\u{a0}x"),
        Some("zw\u{200b}x"),
        Some("ls\u{2028}x"),
        Some("e\u{301}"),
        Some("repl\u{fffd}x"),
        Some("bidi\u{202e}x"),
        Some("c1\u{85}x"),
        Some("{brace}"),
        Some("-dash"),
        Some("日本語"),
        Some("\u{1F600}\u{0}"),
        Some("tab\tand space"),
        Some("trailing "),
        Some(" leading"),
    ];
    let mut cases: Vec<Option<Case>> = strings
        .iter()
        .map(|value| value.map(|value| case(Level::INFO, value, vec![Attr::string("s", value)])))
        .collect();

    cases.extend(value_cases().into_iter().map(Some));
    cases.extend(time_and_error_cases().into_iter().map(Some));
    cases.push(None); // "invalid utf8 slice": []string{"\xff"}
    cases.extend(group_level_and_with_cases().into_iter().map(Some));
    cases
}

/// The records of keys, ints, floats and bools.
fn value_cases() -> Vec<Case> {
    // A variable in Go too, so 0.1 + 0.2 is computed at run time rather than folded to 0.3.
    let tenth = 0.1_f64;
    vec![
        case(
            Level::INFO,
            "keys",
            vec![
                Attr::string("with space", "v"),
                Attr::string("", "emptykey"),
                Attr::string("a=b", "v"),
                Attr::string(r#"q"k"#, "v"),
                Attr::string("dot.key", "v"),
                Attr::string("k\nnl", "v"),
            ],
        ),
        case(
            Level::INFO,
            "ints",
            vec![
                Attr::int("zero", 0),
                Attr::int("neg", -42),
                Attr::int("min", i64::MIN),
                Attr::int("max", i64::MAX),
                Attr::uint("umax", u64::MAX),
                Attr::uint("u", 7),
            ],
        ),
        case(
            Level::INFO,
            "floats",
            vec![
                Attr::float("half", 0.5),
                Attr::float("big", 1e21),
                Attr::float("e20", 1e20),
                Attr::float("small", 1e-7),
                Attr::float("e6", 1e-6),
                Attr::float("mil", 123_456_789.0),
                Attr::float("negzero", -0.0),
                Attr::float("sum", 0.3),
                Attr::float("rsum", tenth + 0.2),
                Attr::float("stamp", 1.729_771_234_5e9),
                Attr::float("one", 1.0),
                Attr::float("maxf", f64::MAX),
                Attr::float("sub", 5e-324),
                // 123456789012345678901.0, as the nearest float64.
                Attr::float("e21m", 1.234_567_890_123_456_8e20),
            ],
        ),
        case(
            Level::INFO,
            "nan",
            vec![Attr::float("nan", f64::NAN), Attr::string("after", "x")],
        ),
        case(
            Level::INFO,
            "inf",
            vec![
                Attr::float("pinf", f64::INFINITY),
                Attr::float("ninf", f64::NEG_INFINITY),
            ],
        ),
        case(
            Level::INFO,
            "bools",
            vec![Attr::bool("t", true), Attr::bool("f", false)],
        ),
    ]
}

/// The records of durations, times, slices and errors.
fn time_and_error_cases() -> Vec<Case> {
    vec![
        case(
            Level::INFO,
            "durations",
            vec![
                Attr::duration("zero", 0),
                Attr::duration("ms", 500_000_000),
                Attr::duration("s30", 30_000_000_000),
                Attr::duration("m1", 60_000_000_000),
                Attr::duration("hms", 3_723_500_000_000),
                Attr::duration("ns", 1),
                Attr::duration("us", 1_500),
                Attr::duration("neg", -2_000_000_000),
                Attr::duration("min", i64::MIN),
                Attr::duration("max", i64::MAX),
                Attr::duration("h100", 360_000_000_000_000),
                Attr::duration("frac", 1_234_567_890),
                Attr::duration("ms1", 1_000_001),
            ],
        ),
        case(
            Level::INFO,
            "times",
            vec![
                Attr::time("cet", record_time(), cet()),
                Attr::time("utc", at(1_767_323_045, 0), Offset::UTC),
                Attr::time("frac", at(1_767_323_045, 120_000_000), Offset::UTC),
                Attr::time(
                    "neg",
                    at(946_704_599, 999_999_999),
                    offset(-(5 * 3600 + 30 * 60)),
                ),
                Attr::time("zero", time_layout::go_zero_time(), Offset::UTC),
            ],
        ),
        case(
            Level::INFO,
            "slices and errors",
            vec![
                Attr::strings("list", ["a", "b c"]),
                Attr::strings("empty", Vec::<String>::new()),
                Attr::strings("one", ["x"]),
                Attr::strings(
                    "ctl",
                    ["a\x08b\x0c", "\x01\x7f", "\u{2028}\u{2029}", "nl\n", ""],
                ),
                Attr::strings("quoted", [r#""q""#, "<&>"]),
                Attr::error("err", &r#"dial tcp: "x" refused"#),
                Attr::error("wrapped", &"service list: boom\nline2"),
                Attr::nil("nilerr"),
            ],
        ),
    ]
}

/// The records of groups, levels and derived loggers.
fn group_level_and_with_cases() -> Vec<Case> {
    vec![
        case(
            Level::INFO,
            "groups",
            vec![
                Attr::group(
                    "req",
                    vec![Attr::string("method", "GET"), Attr::int("n", 2)],
                ),
                Attr::group("a", vec![Attr::group("b", vec![Attr::int("c", 1)])]),
                Attr::group("empty", vec![]),
                Attr::group("", vec![Attr::int("inl", 1)]),
                Attr::group("sp ace", vec![Attr::string("k", "v w")]),
            ],
        ),
        case(Level::DEBUG, "debug", vec![]),
        case(Level::INFO, "info", vec![]),
        case(Level::WARN, "warn", vec![]),
        case(Level::ERROR, "error", vec![]),
        case(Level(1), "info+1", vec![]),
        case(Level(-5), "debug-1", vec![]),
        case(Level(12), "error+4", vec![]),
        case(Level(3), "warn-1", vec![]),
        with_case("with", vec![Attr::int("y", 2)], |logger| {
            logger
                .with_attrs(&[Attr::string("component", "reconciler")])
                .with_group("g")
                .with_attrs(&[Attr::int("x", 1)])
        }),
        with_case("with empty group", vec![], |logger| {
            logger
                .with_attrs(&[Attr::string("c", "d")])
                .with_group("eg")
        }),
        with_case(
            "nested with groups",
            vec![Attr::group("h", vec![Attr::int("i", 3)])],
            |logger| logger.with_group("g1").with_group("g2"),
        ),
        case(Level::INFO, "", vec![Attr::int("k", 1)]),
    ]
}

fn compare(format: Format, include_time: bool, file: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/logger")
        .join(file);
    let golden = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let golden: Vec<&str> = golden.lines().collect();
    let cases = corpus();
    assert_eq!(golden.len(), cases.len(), "{file}: one line per record");

    let mut compared = 0;
    let mut mismatches = Vec::new();
    for (index, (case, want)) in cases.iter().zip(&golden).enumerate() {
        let Some(case) = case else {
            continue;
        };
        let sink = MemorySink::default();
        let base = Logger::new(
            Config {
                format,
                level: Level(-100),
                include_time,
            },
            Box::new(sink.clone()),
            TimeZone::fixed(cet()),
            Arc::new(record_time),
        );
        let logger = case.with.map_or_else(|| base.clone(), |with| with(&base));
        logger.log(case.level, &case.message, &case.attrs);
        let got = sink.contents();
        if got != format!("{want}\n") {
            mismatches.push(format!(
                "record {index}:\n  Go   {want}\n  Rust {}",
                got.trim_end()
            ));
        }
        compared += 1;
    }
    assert!(
        mismatches.is_empty(),
        "{file}: {} of {compared} records differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert_eq!(compared, 51, "{file}");
}

#[test]
fn text_with_time_matches_go() {
    compare(Format::Text, true, "text_time.log");
}

#[test]
fn text_without_time_matches_go() {
    compare(Format::Text, false, "text_notime.log");
}

#[test]
fn json_with_time_matches_go() {
    compare(Format::Json, true, "json_time.log");
}

#[test]
fn json_without_time_matches_go() {
    compare(Format::Json, false, "json_notime.log");
}

#[test]
fn plain_with_time_matches_go() {
    compare(Format::Plain, true, "plain_time.log");
}

#[test]
fn plain_without_time_matches_go() {
    compare(Format::Plain, false, "plain_notime.log");
}
