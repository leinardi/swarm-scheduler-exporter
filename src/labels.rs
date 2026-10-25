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

//! Sanitizing and validating Prometheus label keys, and warning about label values that look
//! high-cardinality.
//!
//! A label name must match `[a-zA-Z_][a-zA-Z0-9_]*`. Sanitizing maps every other character to `_`
//! one per character, as Go does per rune: names are taken as bytes, because a Go string (and a
//! command-line argument) may hold invalid UTF-8, and Go counts each invalid byte as one rune.
//! Label values are never sanitized.
//!
//! The high-cardinality warning goes to the global logger, once per label key.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::logger::{self, Attr};

/// A reasonable upper bound on a sanitized name's length; Prometheus has no hard limit.
const MAX_LABEL_NAME_LEN: usize = 100;

/// A guardrail against unbounded cardinality.
const MAX_CUSTOM_LABEL_COUNT: usize = 8;

/// How many bytes of a value the high-cardinality warning shows.
const HIGH_CARDINALITY_SAMPLE_SIZE: usize = 64;

/// The message of the high-cardinality warning, logged at warn level.
pub const HIGH_CARDINALITY_MESSAGE: &str =
    "label appears high-cardinality; consider avoiding IDs/hashes as metric labels";

/// The label keys already warned about, process-wide. Keys are label names, bounded by the custom
/// labels and the exporter's own label set, so the set needs no eviction.
static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Sanitizes every name, in order (see [`sanitize_name`]).
#[must_use]
pub fn sanitize_label_names<N: AsRef<[u8]>>(names: &[N]) -> Vec<String> {
    names.iter().map(sanitize_name).collect()
}

/// Sanitizes the keys of a label set; the values pass through unchanged. Two keys that sanitize to
/// the same name collapse into one, the later in iteration order winning, as with Go's map
/// assignment (where that order is random).
#[must_use]
pub fn sanitize_metric_labels<K, V, I>(labels: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<[u8]>,
    V: Into<String>,
{
    labels
        .into_iter()
        .map(|(key, value)| (sanitize_name(key), value.into()))
        .collect()
}

/// Sanitizes, then validates the names: length, the reserved `__` prefix, the Prometheus name
/// rule and collisions after sanitizing, checked in that order per name, and the first failure is
/// returned. No names give an empty list.
///
/// # Errors
///
/// A [`LabelError`] naming the first name that fails and the reason.
pub fn validate_and_sanitize_label_names<N: AsRef<[u8]>>(
    names: &[N],
) -> Result<Vec<String>, LabelError> {
    let sanitized_names = sanitize_label_names(names);
    let mut seen = HashSet::with_capacity(sanitized_names.len());

    for (original, sanitized) in names.iter().zip(&sanitized_names) {
        let fail = |reason| Err(LabelError::new(reason, original.as_ref(), sanitized));

        if sanitized.is_empty() || sanitized.len() > MAX_LABEL_NAME_LEN {
            return fail("invalid label name length");
        }
        if sanitized.starts_with("__") {
            return fail("label name uses reserved prefix '__'");
        }
        if !is_valid_label_name(sanitized) {
            return fail("label name violates Prometheus constraints");
        }
        if !seen.insert(sanitized.as_str()) {
            return fail("label name collides after sanitization");
        }
    }

    Ok(sanitized_names)
}

/// Enforces the maximum number of custom label keys.
///
/// # Errors
///
/// A [`CountError`] when `count` is above the maximum.
pub fn validate_custom_label_count(count: usize) -> Result<(), CountError> {
    if count > MAX_CUSTOM_LABEL_COUNT {
        return Err(CountError {
            count,
            maximum: MAX_CUSTOM_LABEL_COUNT,
        });
    }
    Ok(())
}

/// Logs a warning, once per label key for the life of the process, when a value looks like a UUID
/// or a long hex token. It never blocks the metric.
pub fn maybe_warn_high_cardinality(label_key: &str, label_value: &str) {
    if label_value.is_empty() || !is_likely_high_cardinality_value(label_value) {
        return;
    }
    log_warn_once(label_key, || HighCardinalityWarning {
        label: label_key.to_owned(),
        sample_value: truncate(label_value, HIGH_CARDINALITY_SAMPLE_SIZE).to_owned(),
    });
}

/// The high-cardinality warning's attributes, in the order they are logged after
/// [`HIGH_CARDINALITY_MESSAGE`]: `label`, then `sample_value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighCardinalityWarning {
    /// The label key.
    pub label: String,
    /// At most the first 64 bytes of the value, cut on a character boundary.
    pub sample_value: String,
}

/// The seam the warning leaves through: it records `key` as warned and, the first time only,
/// builds the warning and hands it to [`emit_warning`].
fn log_warn_once(key: &str, warning: impl FnOnce() -> HighCardinalityWarning) {
    let first = WARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key.to_owned());
    if first {
        emit_warning(&warning());
    }
}

/// Logs the warning at warn level on the global logger (the tests also capture it).
fn emit_warning(warning: &HighCardinalityWarning) {
    #[cfg(test)]
    tests::EMITTED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(warning.clone());
    logger::global().warn(
        HIGH_CARDINALITY_MESSAGE,
        &[
            Attr::string("label", &warning.label),
            Attr::string("sample_value", &warning.sample_value),
        ],
    );
}

/// Maps `name` to a label name: every character outside `[A-Za-z0-9_]` becomes one `_`, non-ASCII
/// letters and invalid bytes included, and `_` is prefixed when the result would start with a
/// digit. The empty name becomes `_`. The result is ASCII.
#[must_use]
pub fn sanitize_name<N: AsRef<[u8]>>(name: N) -> String {
    let bytes = name.as_ref();
    if bytes.is_empty() {
        return "_".to_owned();
    }

    let mut out = String::with_capacity(bytes.len() + 1);
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        out.push(if is_label_name_byte(byte) {
            char::from(byte)
        } else {
            '_'
        });
        index += go_rune_len(&bytes[index..]);
    }

    // The output is ASCII and not empty, so its first byte is its first character.
    if !is_label_name_start_byte(out.as_bytes()[0]) {
        out.insert(0, '_');
    }
    out
}

/// The length of the rune at the start of `bytes` the way Go's `utf8.DecodeRune` reads it: a
/// complete, valid UTF-8 sequence counts whole, and anything else (an invalid lead byte, a stray
/// continuation byte, a truncated, overlong or surrogate sequence) counts one byte. This is not
/// what Rust's lossy decoding does: `String::from_utf8_lossy` replaces a truncated sequence such
/// as `e2 82` with one U+FFFD, where Go reads two runes.
fn go_rune_len(bytes: &[u8]) -> usize {
    let width = match bytes.first() {
        Some(0xC2..=0xDF) => 2,
        Some(0xE0..=0xEF) => 3,
        Some(0xF0..=0xF4) => 4,
        Some(_) | None => return 1,
    };
    match bytes.get(..width) {
        Some(sequence) if std::str::from_utf8(sequence).is_ok() => width,
        Some(_) | None => 1,
    }
}

/// Whether `name` matches `[a-zA-Z_][a-zA-Z0-9_]*`.
fn is_valid_label_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(is_label_name_start_byte) && bytes.all(is_label_name_byte)
}

/// Whether `byte` may start a label name: `[A-Za-z_]`.
fn is_label_name_start_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

/// Whether `byte` may appear in a label name: `[A-Za-z0-9_]`.
fn is_label_name_byte(byte: u8) -> bool {
    is_label_name_start_byte(byte) || byte.is_ascii_digit()
}

/// At most `max_length` bytes of `input`, cut on a character boundary so a multi-byte character
/// is never split.
fn truncate(input: &str, max_length: usize) -> &str {
    if input.len() <= max_length {
        return input;
    }
    let mut cut = max_length;
    while !input.is_char_boundary(cut) {
        cut -= 1;
    }
    &input[..cut]
}

/// A heuristic for the usual high-cardinality culprits: a UUID shape (8-4-4-4-12 hex digits with
/// dashes) or 16 or more hex digits, in either case.
///
/// Go lowercases with `strings.ToLower`, which differs from an ASCII lowercase only on non-ASCII
/// characters, and no non-ASCII character lowercases to a hex digit or a dash in either: a value
/// holding one is not a match either way, so the ASCII lowercase is equivalent here.
fn is_likely_high_cardinality_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let bytes = lower.as_bytes();

    let is_uuid = bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|&dash| bytes[dash] == b'-')
        && [(0, 8), (9, 13), (14, 18), (19, 23), (24, 36)]
            .iter()
            .all(|&(start, end)| is_hex_string(&lower[start..end]));
    if is_uuid {
        return true;
    }

    bytes.len() >= 16 && is_hex_string(&lower)
}

/// Whether every character is a lowercase hex digit; the empty string is.
fn is_hex_string(hex: &str) -> bool {
    hex.chars()
        .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

/// A label name that failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelError {
    reason: &'static str,
    original: String,
    sanitized: String,
}

impl LabelError {
    fn new(reason: &'static str, original: &[u8], sanitized: &str) -> Self {
        Self {
            reason,
            // The message is text: an argument that is not UTF-8 shows U+FFFD where Go would
            // print the raw byte.
            original: String::from_utf8_lossy(original).into_owned(),
            sanitized: sanitized.to_owned(),
        }
    }
}

impl fmt::Display for LabelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid label: {} (original={}, sanitized={})",
            self.reason, self.original, self.sanitized
        )
    }
}

impl std::error::Error for LabelError {}

/// More custom label keys than allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountError {
    count: usize,
    maximum: usize,
}

impl fmt::Display for CountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "too many custom labels: {} (max {})",
            self.count, self.maximum
        )
    }
}

impl std::error::Error for CountError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every warning emitted, in order, across the tests of this process.
    pub(super) static EMITTED: Mutex<Vec<HighCardinalityWarning>> = Mutex::new(Vec::new());

    fn warned(key: &str) -> bool {
        WARNED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    /// The warnings emitted for `key`. Keys are unique per test: the sets are process-wide and
    /// the tests run in parallel.
    fn emitted(key: &str) -> Vec<HighCardinalityWarning> {
        EMITTED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|warning| warning.label == key)
            .cloned()
            .collect()
    }

    /// Go: `TestSanitizeName`
    #[test]
    fn sanitize_name_maps_each_rune_to_one_character() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "_"),
            (b"foo", "foo"),
            (b"foo_bar", "foo_bar"),
            (b"foo-bar", "foo_bar"),
            (b"foo.bar", "foo_bar"),
            (b"foo bar", "foo_bar"),
            (b"123foo", "_123foo"),
            (b"_leading", "_leading"),
            ("Ünïcödé".as_bytes(), "_n_c_d_"),
            (b"__reserved", "__reserved"),
            // Non-ASCII letters are not valid in a label name: each rune becomes '_'.
            ("équipe".as_bytes(), "_quipe"),
            ("tëam".as_bytes(), "t_am"),
            ("команда".as_bytes(), "_______"),
            ("\u{56e2}\u{961f}".as_bytes(), "__"),
            ("9\u{5718}".as_bytes(), "_9_"),
            // An invalid byte is a rune too.
            (b"a\xffb", "a_b"),
            // Go reads a truncated sequence one byte at a time: two runes, where Rust's lossy
            // decoding would see one replacement character.
            (b"a\xe2\x82b", "a__b"),
            // A stray continuation byte, an overlong encoding and a surrogate: one rune per byte.
            // These and the cases below were checked against Go's sanitizeName.
            (b"a\x80b", "a_b"),
            (b"a\xc0\xafb", "a__b"),
            (b"a\xed\xa0\x80b", "a___b"),
            // Above U+10FFFF, and a truncated sequence alone (no prefix: it starts with '_').
            (b"a\xf4\x90\x80\x80b", "a____b"),
            (b"\xe2\x82", "__"),
        ];
        for &(input, want) in cases {
            assert_eq!(sanitize_name(input), want, "sanitize_name({input:?})");
        }
    }

    /// Go: `TestSanitizeLabelNames`
    #[test]
    fn sanitize_label_names_keeps_the_order() {
        assert_eq!(
            sanitize_label_names(&["foo", "foo-bar", "123bad", ""]),
            ["foo", "foo_bar", "_123bad", "_"]
        );
    }

    /// Go: `TestSanitizeMetricLabels_KeysSanitized_ValuesPreserved`
    #[test]
    fn sanitize_metric_labels_sanitizes_keys_and_keeps_values() {
        let got = sanitize_metric_labels([("foo-bar", "my value"), ("ok_key", "other")]);
        assert_eq!(got.get("foo_bar").map(String::as_str), Some("my value"));
        assert_eq!(got.get("ok_key").map(String::as_str), Some("other"));
        assert_eq!(got.len(), 2);
    }

    /// Go: `TestValidateAndSanitizeLabelNames`
    #[test]
    fn validate_and_sanitize_label_names_checks_in_order() {
        // empty input returns nothing
        let empty: [&str; 0] = [];
        assert_eq!(validate_and_sanitize_label_names(&empty), Ok(vec![]));

        // valid names returned sanitized
        assert_eq!(
            validate_and_sanitize_label_names(&["foo", "bar_baz"]),
            Ok(vec!["foo".to_owned(), "bar_baz".to_owned()])
        );

        // reserved __ prefix errors
        let error = validate_and_sanitize_label_names(&["__reserved"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid label: label name uses reserved prefix '__' (original=__reserved, \
             sanitized=__reserved)"
        );

        // too long errors; exactly the maximum is accepted
        let long_name = "a".repeat(MAX_LABEL_NAME_LEN + 1);
        let error = validate_and_sanitize_label_names(&[long_name.as_str()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "invalid label: invalid label name length (original={long_name}, \
                 sanitized={long_name})"
            )
        );
        let max_name = "a".repeat(MAX_LABEL_NAME_LEN);
        assert!(validate_and_sanitize_label_names(&[max_name.as_str()]).is_ok());

        // non-ASCII letter sanitized and accepted
        assert_eq!(
            validate_and_sanitize_label_names(&["équipe"]),
            Ok(vec!["_quipe".to_owned()])
        );

        // leading non-ASCII runs hit the reserved prefix: two or more non-ASCII characters first
        // sanitize to "__...", which is reserved.
        for name in ["команда", "ÜÖteam"] {
            let error = validate_and_sanitize_label_names(&[name]).unwrap_err();
            assert!(
                error.to_string().contains("reserved prefix"),
                "validate_and_sanitize_label_names({name:?}): {error}"
            );
        }

        // non-ASCII collision errors: both sanitize to the same name
        for name in ["tëam", "t_am"] {
            assert_eq!(sanitize_name(name), "t_am");
        }
        let error = validate_and_sanitize_label_names(&["tëam", "t_am"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid label: label name collides after sanitization (original=t_am, sanitized=t_am)"
        );

        // post-sanitize collision errors (both become foo_bar)
        let error = validate_and_sanitize_label_names(&["foo.bar", "foo-bar"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid label: label name collides after sanitization (original=foo-bar, \
             sanitized=foo_bar)"
        );
    }

    /// Go: `TestValidateCustomLabelCount`
    #[test]
    fn validate_custom_label_count_allows_up_to_the_maximum() {
        assert_eq!(validate_custom_label_count(MAX_CUSTOM_LABEL_COUNT), Ok(()));
        let error = validate_custom_label_count(MAX_CUSTOM_LABEL_COUNT + 1).unwrap_err();
        assert_eq!(error.to_string(), "too many custom labels: 9 (max 8)");
    }

    /// Go: `TestIsLikelyHighCardinalityValue`
    #[test]
    fn is_likely_high_cardinality_value_matches_uuids_and_long_hex() {
        let cases = [
            ("550e8400-e29b-41d4-a716-446655440000", true), // UUID v4 shape
            ("AABBCCDDEEFF00112233445566778899", true),     // 32 hex uppercase
            ("aabbccddeeff001122334455", true),             // 24 hex, at least 16
            ("aabbccddeeff0011", true),                     // 16 hex
            ("aabbccddeeff001", false),                     // 15 hex, shorter than 16
            ("hello", false),                               // not hex, short
            ("not-a-uuid-but-dashed", false),
            ("", false),
            // A UUID shape with a non-hex digit, or with a non-ASCII character in place of one.
            ("550e8400-e29b-41d4-a716-44665544000g", false),
            ("550e8400-e29b-41d4-a716-4466554400é", false),
        ];
        for (input, want) in cases {
            assert_eq!(
                is_likely_high_cardinality_value(input),
                want,
                "is_likely_high_cardinality_value({input:?})"
            );
        }
    }

    /// Go: `TestIsHexString`
    #[test]
    fn is_hex_string_accepts_lowercase_hex_only() {
        let cases = [
            ("0123456789abcdef", true),
            ("ABCDEF", false), // uppercase is not in the range checked
            ("xyz", false),
            ("", true), // vacuous
        ];
        for (input, want) in cases {
            assert_eq!(is_hex_string(input), want, "is_hex_string({input:?})");
        }
    }

    /// Go: `TestTruncate_RuneBoundary`
    #[test]
    fn truncate_cuts_on_a_character_boundary() {
        // Byte 2 is inside "é": a byte cut would split it.
        assert_eq!(truncate("aéz", 2), "a");
    }

    /// Go: `TestTruncate`
    #[test]
    fn truncate_passes_short_input_through() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    /// Go: `TestMaybeWarnHighCardinality_WarnedOnce`
    #[test]
    fn maybe_warn_high_cardinality_warns_once_per_key() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        maybe_warn_high_cardinality("warned_once_key", uuid);
        assert!(warned("warned_once_key"));

        // The second call finds the key and emits nothing.
        maybe_warn_high_cardinality("warned_once_key", uuid);
        assert!(warned("warned_once_key"));
        assert_eq!(
            emitted("warned_once_key"),
            [HighCardinalityWarning {
                label: "warned_once_key".to_owned(),
                sample_value: uuid.to_owned(),
            }]
        );
    }

    /// Go: `TestMaybeWarnHighCardinality_NoWarnForLowCardinality`
    #[test]
    fn maybe_warn_high_cardinality_ignores_low_cardinality_values() {
        maybe_warn_high_cardinality("low_key", "simple_value");
        assert!(!warned("low_key"));
        assert!(emitted("low_key").is_empty());
    }

    /// Go: `TestMaybeWarnHighCardinality_EmptyValueSkipped`
    #[test]
    fn maybe_warn_high_cardinality_skips_an_empty_value() {
        maybe_warn_high_cardinality("empty_key", "");
        assert!(!warned("empty_key"));
        assert!(emitted("empty_key").is_empty());
    }

    /// The warning reaches the global logger at warn level, with Go's message and attributes.
    #[test]
    fn high_cardinality_warning_is_logged_at_warn() {
        let sink = logger::MemorySink::default();
        logger::set_global(logger::Logger::new(
            logger::Config {
                format: logger::Format::Text,
                level: logger::Level::INFO,
                include_time: false,
            },
            Box::new(sink.clone()),
            jiff::tz::TimeZone::UTC,
            std::sync::Arc::new(crate::go_compat::time_layout::go_zero_time),
        ));
        maybe_warn_high_cardinality("logged_key", "aabbccddeeff0011");
        let line = "level=WARN msg=\"label appears high-cardinality; consider avoiding IDs/hashes as \
                    metric labels\" label=logged_key sample_value=aabbccddeeff0011\n";
        assert!(sink.contents().contains(line), "{:?}", sink.contents());
    }

    /// The warning's sample is the value cut to its first 64 bytes, and the message is Go's.
    #[test]
    fn high_cardinality_warning_samples_at_most_64_bytes() {
        let value = "ab".repeat(40);
        maybe_warn_high_cardinality("sample_key", &value);
        let warnings = emitted("sample_key");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].sample_value, "ab".repeat(32));
        assert_eq!(
            HIGH_CARDINALITY_MESSAGE,
            "label appears high-cardinality; consider avoiding IDs/hashes as metric labels"
        );
    }
}
