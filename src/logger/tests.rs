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

use super::plain_handler::{level_to_upper, qualify};
use super::*;
use crate::gofmt_compat::quote;

/// A logger writing to a memory sink, with a fixed record time of 2025-01-15T10:00:00Z.
fn logger(format: Format, level: Level, include_time: bool) -> (Logger, MemorySink) {
    let sink = MemorySink::default();
    let logger = Logger::new(
        Config {
            format,
            level,
            include_time,
        },
        Box::new(sink.clone()),
        TimeZone::UTC,
        Arc::new(|| Timestamp::new(1_736_935_200, 0).unwrap()),
    );
    (logger, sink)
}

/// Go's `newTestHandler`: plain, no time.
fn plain(level: Level) -> (Logger, MemorySink) {
    logger(Format::Plain, level, false)
}

/// Go: `TestParseLevel`, with the Q4 rejections in place of Go's silent fallback to info.
#[test]
fn parse_level_accepts_the_names_in_any_case() {
    let cases = [
        ("debug", Level::DEBUG),
        ("DEBUG", Level::DEBUG),
        ("info", Level::INFO),
        ("INFO", Level::INFO),
        ("warn", Level::WARN),
        ("WARN", Level::WARN),
        ("warning", Level::WARN),
        ("error", Level::ERROR),
        ("fatal", Level::ERROR),
        ("panic", Level::ERROR),
        // Go's strings.ToLower maps U+0130 to a plain i.
        ("\u{130}NFO", Level::INFO),
    ];
    for (input, want) in cases {
        assert_eq!(parse_level(input), Ok(want), "parse_level({input:?})");
    }
    for input in ["", "unknown", "VERBOSE", "trace", "info+1", " info"] {
        let error = parse_level(input).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "invalid log level {input:?} (want one of debug, info, warn, warning, error, \
                 fatal, panic)"
            )
        );
    }
}

/// Q4: the format names, in any case, and nothing else.
#[test]
fn parse_format_accepts_the_names_in_any_case() {
    for (input, want) in [
        ("json", Format::Json),
        ("JSON", Format::Json),
        ("text", Format::Text),
        ("Text", Format::Text),
        ("plain", Format::Plain),
        ("PLAIN", Format::Plain),
    ] {
        assert_eq!(parse_format(input), Ok(want), "parse_format({input:?})");
    }
    for input in ["", "logfmt", "yaml"] {
        assert_eq!(
            parse_format(input).unwrap_err().to_string(),
            format!("invalid log format {input:?} (want one of json, text, plain)")
        );
    }
}

/// Go: `TestTimeStripper_IncludeTime_ReturnsNil`: with the time on, nothing is stripped, a
/// user attribute keyed `time` included.
#[test]
fn include_time_keeps_every_time() {
    let (text, sink) = logger(Format::Text, Level::INFO, true);
    text.info("m", &[Attr::string("time", "x")]);
    assert_eq!(
        sink.contents(),
        "time=2025-01-15T10:00:00.000Z level=INFO msg=m time=x\n"
    );
}

/// Go: `TestTimeStripper_ExcludeTime_DropsTimeKey`: the record time and every attribute keyed
/// `time` are dropped, groups included, with slog's quirks when that empties a group (the text
/// prefix stays, and JSON loses a comma). Expected lines are Go's output.
#[test]
fn exclude_time_drops_every_time_key() {
    let cases: [(&str, Vec<Attr>, &str, &str); 4] = [
        (
            "top",
            vec![Attr::string("time", "x"), Attr::int("k", 1)],
            "level=INFO msg=top k=1",
            r#"{"level":"INFO","msg":"top","k":1}"#,
        ),
        (
            "group",
            vec![
                Attr::group("g", vec![Attr::string("time", "x")]),
                Attr::int("k", 1),
            ],
            "level=INFO msg=group g.k=1",
            r#"{"level":"INFO","msg":"group""k":1}"#,
        ),
        (
            "only",
            vec![Attr::string("time", "x")],
            "level=INFO msg=only",
            r#"{"level":"INFO","msg":"only"}"#,
        ),
        (
            "nested",
            vec![Attr::group(
                "g",
                vec![Attr::string("time", "x"), Attr::int("a", 2)],
            )],
            "level=INFO msg=nested g.a=2",
            r#"{"level":"INFO","msg":"nested","g":{"a":2}}"#,
        ),
    ];
    for (message, attrs, want_text, want_json) in cases {
        let (text, text_sink) = logger(Format::Text, Level::INFO, false);
        text.info(message, &attrs);
        assert_eq!(
            text_sink.contents(),
            format!("{want_text}\n"),
            "text {message}"
        );
        let (json, json_sink) = logger(Format::Json, Level::INFO, false);
        json.info(message, &attrs);
        assert_eq!(
            json_sink.contents(),
            format!("{want_json}\n"),
            "json {message}"
        );
    }

    let (text, sink) = logger(Format::Text, Level::INFO, false);
    text.with_attrs(&[Attr::string("time", "w"), Attr::string("c", "d")])
        .info("with", &[]);
    text.with_group("wg")
        .info("withgroup", &[Attr::string("time", "x")]);
    text.with_group("wg").info(
        "withgroup2",
        &[Attr::string("time", "x"), Attr::int("k", 1)],
    );
    assert_eq!(
        sink.contents(),
        "level=INFO msg=with c=d\nlevel=INFO msg=withgroup\nlevel=INFO msg=withgroup2 wg.k=1\n"
    );
}

/// Go: `TestTimeStripper_ExcludeTime_PassesOtherAttrs`.
#[test]
fn exclude_time_passes_other_attrs() {
    let (text, sink) = logger(Format::Text, Level::INFO, false);
    text.info("m", &[Attr::string("service", "myapp")]);
    assert_eq!(sink.contents(), "level=INFO msg=m service=myapp\n");
}

/// Go: `TestPlainHandler_BasicLine`
#[test]
fn plain_basic_line() {
    let (logger, sink) = plain(Level::INFO);
    logger.info("hello world", &[]);
    assert_eq!(sink.contents(), "level=INFO hello world\n");
}

/// Go: `TestPlainHandler_WithAttr`
#[test]
fn plain_with_attr() {
    let (logger, sink) = plain(Level::INFO);
    logger.info("msg", &[Attr::string("k", "v")]);
    assert_eq!(sink.contents(), "level=INFO msg k=v\n");
}

/// Go: `TestPlainHandler_QuotedValue`
#[test]
fn plain_quoted_value() {
    let (logger, sink) = plain(Level::INFO);
    logger.info("msg", &[Attr::string("k", "hello world")]);
    assert_eq!(sink.contents(), "level=INFO msg k=\"hello world\"\n");
}

/// Go: `TestPlainHandler_LevelVariants`
#[test]
fn plain_level_variants() {
    for (level, want) in [
        (Level::DEBUG, "DEBUG"),
        (Level::INFO, "INFO"),
        (Level::WARN, "WARN"),
        (Level::ERROR, "ERROR"),
    ] {
        let (logger, sink) = plain(level);
        logger.log(level, "", &[]);
        assert_eq!(sink.contents(), format!("level={want}\n"), "{level}");
    }
}

/// Go: `TestPlainHandler_Enabled`
#[test]
fn plain_enabled() {
    let (logger, _) = plain(Level::WARN);
    assert!(!logger.enabled(Level::INFO));
    assert!(logger.enabled(Level::WARN));
    assert!(logger.enabled(Level::ERROR));
}

/// Go: `TestPlainHandler_WithGroup`
#[test]
fn plain_with_group() {
    let (logger, sink) = plain(Level::INFO);
    logger
        .with_group("g1")
        .info("msg", &[Attr::string("k", "v")]);
    assert_eq!(sink.contents(), "level=INFO msg g1.k=v\n");
}

/// Go: `TestPlainHandler_WithAttrs`
#[test]
fn plain_with_attrs() {
    let (logger, sink) = plain(Level::INFO);
    logger
        .with_attrs(&[Attr::string("prekey", "preval")])
        .info("msg", &[Attr::string("k", "v")]);
    assert_eq!(sink.contents(), "level=INFO msg prekey=preval k=v\n");
}

/// Go: `TestPlainHandler_WithAttrs_Empty_ReturnsSelf`
#[test]
fn plain_with_no_attrs_returns_the_same_logger() {
    let (logger, _) = plain(Level::INFO);
    let derived = logger.with_attrs(&[]);
    assert!(Arc::ptr_eq(&logger.handler, &derived.handler));
}

/// Go: `TestPlainHandler_WithGroup_EmptyName_ReturnsSelf`: a blank name is trimmed to nothing
/// and changes nothing.
#[test]
fn plain_with_blank_group_changes_nothing() {
    let (logger, sink) = plain(Level::INFO);
    let derived = logger.with_group("   ");
    assert_eq!(derived.handler, logger.handler);
    derived.info("msg", &[Attr::string("k", "v")]);
    assert_eq!(sink.contents(), "level=INFO msg k=v\n");
    assert!(Arc::ptr_eq(&logger.with_group("").handler, &logger.handler));
}

/// Go: `TestPlainHandler_OriginalUnmutatedByWithAttrs`
#[test]
fn plain_original_unchanged_by_with_attrs() {
    let (original, sink) = plain(Level::INFO);
    let derived = original.with_attrs(&[Attr::string("extra", "x")]);
    original.info("msg", &[]);
    derived.info("msg", &[]);
    assert_eq!(sink.contents(), "level=INFO msg\nlevel=INFO msg extra=x\n");
}

/// Go: `TestPlainHandler_GroupAttr`
#[test]
fn plain_group_attr() {
    let (logger, sink) = plain(Level::INFO);
    logger.info("msg", &[Attr::group("g", vec![Attr::int("n", 42)])]);
    assert_eq!(sink.contents(), "level=INFO msg g={g.n=42}\n");
}

/// Go: `TestPlainHandler_IncludeTime`
#[test]
fn plain_include_time() {
    let (logger, sink) = logger(Format::Plain, Level::INFO, true);
    logger.info("msg", &[]);
    assert_eq!(
        sink.contents(),
        "time=2025-01-15T10:00:00Z level=INFO msg\n"
    );
}

/// Go: `TestLevelToUpper`
#[test]
fn plain_level_to_upper() {
    for (level, want) in [
        (Level::DEBUG, "DEBUG"),
        (Level(-5), "DEBUG"),
        (Level::INFO, "INFO"),
        (Level::WARN, "WARN"),
        (Level::ERROR, "ERROR"),
        (Level(9), "ERROR"),
        // Not exactly info or warn: the plain format reads ERROR.
        (Level(1), "ERROR"),
    ] {
        assert_eq!(level_to_upper(level), want, "{level}");
    }
}

/// Go: `TestQualify_NoGroups`
#[test]
fn plain_qualify_without_groups() {
    assert_eq!(qualify(&[], &Attr::string("key", "val")).key, "key");
}

/// Go: `TestQualify_WithGroups`
#[test]
fn plain_qualify_with_groups() {
    let groups = ["g1".to_owned(), "g2".to_owned()];
    assert_eq!(qualify(&groups, &Attr::string("k", "v")).key, "g1.g2.k");
}

/// Where an escaping case puts its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Message,
    TopKey,
    GroupName,
    GroupChildKey,
    StringValue,
    ErrorValue,
    GroupValue,
}

const TEXT_POSITIONS: [Position; 6] = [
    Position::Message,
    Position::TopKey,
    Position::GroupChildKey,
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
];
/// Every position; the group name is trimmed by `with_group`.
const ALL_POSITIONS: [Position; 7] = [
    Position::Message,
    Position::TopKey,
    Position::GroupChildKey,
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
    Position::GroupName,
];
const KEY_POSITIONS: [Position; 3] = [
    Position::TopKey,
    Position::GroupName,
    Position::GroupChildKey,
];
const VALUE_POSITIONS: [Position; 3] = [
    Position::StringValue,
    Position::ErrorValue,
    Position::GroupValue,
];

/// Logs `text` at `position` and returns the line.
fn log_at(position: Position, text: &str) -> String {
    let (base, sink) = plain(Level::INFO);
    let mut logger = base.clone();
    let mut message = "msg";
    let attrs = match position {
        Position::Message => {
            message = text;
            vec![Attr::int("after", 1)]
        }
        Position::TopKey => vec![Attr::string(text, "v")],
        Position::GroupName => {
            logger = base.with_group(text);
            vec![Attr::string("k", "v")]
        }
        Position::GroupChildKey => vec![Attr::group("g", vec![Attr::string(text, "v")])],
        Position::StringValue => vec![Attr::string("k", text)],
        Position::ErrorValue => vec![Attr::error("err", &text)],
        Position::GroupValue => vec![Attr::group("g", vec![Attr::string("k", text)])],
    };
    logger.info(message, &attrs);
    sink.contents()
}

/// The exact line `log_at(position, text)` must produce, from the escaped message and the quoted
/// form of the text.
fn want_at(position: Position, message: &str, quoted: &str) -> String {
    let inner = &quoted[1..quoted.len() - 1];
    match position {
        Position::Message => format!("level=INFO {message} after=1\n"),
        Position::TopKey => format!("level=INFO msg {quoted}=v\n"),
        Position::GroupName => format!("level=INFO msg \"{inner}.k\"=v\n"),
        Position::GroupChildKey => format!("level=INFO msg g={{\"g.{inner}\"=v}}\n"),
        Position::StringValue => format!("level=INFO msg k={quoted}\n"),
        Position::ErrorValue => format!("level=INFO msg err={quoted}\n"),
        Position::GroupValue => format!("level=INFO msg g={{g.k={quoted}}}\n"),
    }
}

/// The escaping cases: a name, the text, its escaped form in the message (empty where the
/// case puts nothing there) and its quoted form, and the positions it is logged at.
type EscapingCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [Position],
);

const ESCAPING_CASES: [EscapingCase; 20] = [
    (
        "forged record",
        "\nlevel=ERROR forged",
        r"\nlevel=ERROR forged",
        r#""\nlevel=ERROR forged""#,
        // with_group trims the leading newline of a group name.
        &TEXT_POSITIONS,
    ),
    (
        "ESC",
        "\x1b[31m",
        r"\x1b[31m",
        r#""\x1b[31m""#,
        &ALL_POSITIONS,
    ),
    (
        "U+0085",
        "a\u{85}b",
        r"a\u0085b",
        r#""a\u0085b""#,
        &ALL_POSITIONS,
    ),
    (
        "U+009B",
        "a\u{9b}b",
        r"a\u009bb",
        r#""a\u009bb""#,
        &ALL_POSITIONS,
    ),
    (
        "U+2028",
        "a\u{2028}b",
        r"a\u2028b",
        r#""a\u2028b""#,
        &ALL_POSITIONS,
    ),
    (
        "U+2029",
        "a\u{2029}b",
        r"a\u2029b",
        r#""a\u2029b""#,
        &ALL_POSITIONS,
    ),
    (
        "bidi override",
        "a\u{202e}b",
        r"a\u202eb",
        r#""a\u202eb""#,
        &ALL_POSITIONS,
    ),
    (
        "quote without a space",
        r#"a"b"#,
        r#"a"b"#,
        r#""a\"b""#,
        &ALL_POSITIONS,
    ),
    (
        "newline without a space",
        "a\nb",
        r"a\nb",
        r#""a\nb""#,
        &ALL_POSITIONS,
    ),
    ("backslash", r"a\b", r"a\\b", r#""a\\b""#, &ALL_POSITIONS),
    (
        "error with spaces",
        "dial tcp: connection refused",
        "",
        r#""dial tcp: connection refused""#,
        &[Position::ErrorValue],
    ),
    ("empty value", "", "", r#""""#, &VALUE_POSITIONS),
    ("empty key", "", "", r#""""#, &[Position::TopKey]),
    (
        "key with a space and =",
        "x level=ERROR",
        "",
        r#""x level=ERROR""#,
        &KEY_POSITIONS,
    ),
    (
        "key with a quote",
        r#"a"b"#,
        "",
        r#""a\"b""#,
        &KEY_POSITIONS,
    ),
    (
        "key with a close brace",
        "a}b",
        "",
        r#""a}b""#,
        &KEY_POSITIONS,
    ),
    (
        "key with an open brace",
        "a{b",
        "",
        r#""a{b""#,
        &KEY_POSITIONS,
    ),
    (
        "group name",
        "g h=1",
        "",
        r#""g h=1""#,
        &[Position::GroupName],
    ),
    ("close brace value", "}", "", r#""}""#, &VALUE_POSITIONS),
    ("open brace value", "{", "", r#""{""#, &VALUE_POSITIONS),
];

/// Go: `TestPlainHandler_Escaping`: hostile text in every position of a plain line gives
/// exactly one line, with exactly the escaped text. Go also parses each line back into its
/// fields; here the exact line is asserted, which pins the same fields. Go's "invalid byte" case
/// has no Rust counterpart: a `&str` cannot hold an invalid byte.
#[test]
fn plain_escaping() {
    for (name, text, message, quoted, positions) in ESCAPING_CASES {
        // The quoted form is strconv.Quote's.
        assert_eq!(quote::quote(text), quoted, "{name}: quoted form");
        for &position in positions {
            let got = log_at(position, text);
            assert_eq!(got.matches('\n').count(), 1, "{name}/{position:?}: {got:?}");
            assert_eq!(
                got,
                want_at(position, message, quoted),
                "{name}/{position:?}"
            );
        }
    }
}
