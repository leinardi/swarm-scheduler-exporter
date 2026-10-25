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

//! A reader for the Prometheus text exposition format, as the integration suite scrapes
//! `/metrics`: every gauge, counter and untyped sample, by name and labels. Histogram and summary
//! samples are skipped, as the Go suite's expfmt-based reader skipped them; the tests assert on
//! none. Also the line format the suite prints samples in (and the snapshot golden file holds):
//! `name{key="value",...} value`, keys sorted, values quoted as Go's `strconv.Quote` does, the
//! number as Go's `strconv.FormatFloat(v, 'g', -1, 64)`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use swarm_scheduler_exporter::gofmt_compat::float::format_g;
use swarm_scheduler_exporter::gofmt_compat::quote::quote;

/// Labels by name, sorted.
pub type Labels = BTreeMap<String, String>;

/// One sample: its metric name, labels and value.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// The metric name.
    pub name: String,
    /// The labels, by name.
    pub labels: Labels,
    /// The value.
    pub value: f64,
}

/// A line the reader cannot take apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    line: usize,
    reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "text format parsing error in line {}: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for ParseError {}

/// One parsed read of `/metrics`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scrape {
    /// Every gauge, counter and untyped sample, in exposition order.
    pub samples: Vec<Sample>,
}

impl Scrape {
    /// Parses an exposition.
    ///
    /// # Errors
    ///
    /// A malformed sample, `TYPE` line or label set.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut types: HashMap<&str, &str> = HashMap::new();
        let mut samples = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let error = |reason: String| ParseError {
                line: index + 1,
                reason,
            };
            let line = raw.trim_matches([' ', '\t']);
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                let mut words = comment.split_whitespace();
                if words.next() == Some("TYPE") {
                    let (Some(name), Some(kind), None) = (words.next(), words.next(), words.next())
                    else {
                        return Err(error("malformed TYPE line".to_owned()));
                    };
                    types.insert(name, kind);
                }
                continue;
            }
            let sample = parse_sample(line).map_err(error)?;
            if !is_skipped(&types, &sample.name) {
                samples.push(sample);
            }
        }
        Ok(Self { samples })
    }

    /// The samples of `name` whose labels include every pair of `matching`.
    #[must_use]
    pub fn find(&self, name: &str, matching: &Labels) -> Vec<&Sample> {
        self.samples
            .iter()
            .filter(|sample| sample.name == name && labels_include(&sample.labels, matching))
            .collect()
    }

    /// The value of the single sample of `name` matching `matching`, if exactly one matches.
    #[must_use]
    pub fn value(&self, name: &str, matching: &Labels) -> Option<f64> {
        match self.find(name, matching).as_slice() {
            [only] => Some(only.value),
            _ => None,
        }
    }

    /// The sum of every sample of `name` matching `matching`.
    #[must_use]
    pub fn sum(&self, name: &str, matching: &Labels) -> f64 {
        self.find(name, matching)
            .iter()
            .map(|sample| sample.value)
            .sum()
    }

    /// Whether any sample of `name` matches `matching`.
    #[must_use]
    pub fn has(&self, name: &str, matching: &Labels) -> bool {
        !self.find(name, matching).is_empty()
    }

    /// Every sample whose `label` is `value`, formatted and sorted, for failure output.
    #[must_use]
    pub fn lines_with(&self, label: &str, value: &str) -> Vec<String> {
        let mut lines: Vec<String> = self
            .samples
            .iter()
            .filter(|sample| sample.labels.get(label).map(String::as_str) == Some(value))
            .map(format_sample)
            .collect();
        lines.sort();
        lines
    }
}

/// Labels from `(key, value)` pairs.
#[must_use]
pub fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Whether every pair of `matching` is in `labels`.
#[must_use]
pub fn labels_include(labels: &Labels, matching: &Labels) -> bool {
    matching
        .iter()
        .all(|(key, wanted)| labels.get(key) == Some(wanted))
}

/// A sample as `name{key="value",...} value`.
#[must_use]
pub fn format_sample(sample: &Sample) -> String {
    format!(
        "{} {}",
        format_series(&sample.name, &sample.labels),
        format_value(sample.value)
    )
}

/// A series as `name{key="value",...}`, keys sorted; `name{}` with no label.
#[must_use]
pub fn format_series(name: &str, labels: &Labels) -> String {
    let pairs: Vec<String> = labels
        .iter()
        .map(|(key, value)| format!("{key}={}", quote(value)))
        .collect();
    format!("{name}{{{}}}", pairs.join(","))
}

/// A value as Go's `strconv.FormatFloat(value, 'g', -1, 64)`.
#[must_use]
pub fn format_value(value: f64) -> String {
    format_g(value)
}

/// Whether a sample belongs to a histogram or summary family.
fn is_skipped(types: &HashMap<&str, &str>, name: &str) -> bool {
    let complex = |family: &str| matches!(types.get(family), Some(&("histogram" | "summary")));
    if complex(name) {
        return true;
    }
    ["_bucket", "_sum", "_count"]
        .iter()
        .filter_map(|suffix| name.strip_suffix(suffix))
        .any(complex)
}

fn is_name_char(character: char, first: bool) -> bool {
    character.is_ascii_alphabetic()
        || character == '_'
        || character == ':'
        || (!first && character.is_ascii_digit())
}

/// `name`, an optional `{labels}`, a value and an optional timestamp.
fn parse_sample(line: &str) -> Result<Sample, String> {
    let name_end = line
        .char_indices()
        .find(|&(index, character)| !is_name_char(character, index == 0))
        .map_or(line.len(), |(index, _)| index);
    if name_end == 0 {
        return Err(format!("invalid metric name in {line:?}"));
    }
    let name = line[..name_end].to_owned();
    let mut rest = &line[name_end..];

    let mut labels = BTreeMap::new();
    if let Some(after) = rest.strip_prefix('{') {
        rest = parse_labels(after, &mut labels)?;
    }

    let mut fields = rest.split_whitespace();
    let value_text = fields
        .next()
        .ok_or_else(|| format!("no value in {line:?}"))?;
    let value = parse_value(value_text).ok_or_else(|| format!("invalid value {value_text:?}"))?;
    if let Some(timestamp) = fields.next()
        && timestamp.parse::<i64>().is_err()
    {
        return Err(format!("invalid timestamp {timestamp:?}"));
    }
    if fields.next().is_some() {
        return Err(format!("unexpected text after the value in {line:?}"));
    }
    Ok(Sample {
        name,
        labels,
        value,
    })
}

/// The labels after `{`, through `}`; returns what follows it.
fn parse_labels<'line>(mut rest: &'line str, labels: &mut Labels) -> Result<&'line str, String> {
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('}') {
            return Ok(after);
        }
        let key_end = rest
            .char_indices()
            .find(|&(index, character)| {
                !(character.is_ascii_alphabetic()
                    || character == '_'
                    || (index > 0 && character.is_ascii_digit()))
            })
            .map_or(rest.len(), |(index, _)| index);
        if key_end == 0 {
            return Err(format!("invalid label name at {rest:?}"));
        }
        let key = rest[..key_end].to_owned();
        rest = rest[key_end..].trim_start();
        rest = rest
            .strip_prefix("=\"")
            .ok_or_else(|| format!("expected =\" after label {key}"))?;
        let (value, after) = parse_label_value(rest, &key)?;
        if labels.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate label {key}"));
        }
        rest = after.trim_start();
        if let Some(after) = rest.strip_prefix(',') {
            rest = after;
        } else if !rest.starts_with('}') {
            return Err(format!("expected , or }} after label {key}"));
        }
    }
}

/// A quoted label value after its opening quote, unescaped, and what follows its closing quote.
fn parse_label_value<'line>(rest: &'line str, key: &str) -> Result<(String, &'line str), String> {
    let unterminated = || format!("unterminated value of label {key}");
    let mut value = String::new();
    let mut chars = rest.char_indices();
    loop {
        match chars.next() {
            Some((index, '"')) => return Ok((value, &rest[index + 1..])),
            Some((_, '\\')) => match chars.next() {
                Some((_, 'n')) => value.push('\n'),
                Some((_, '\\')) => value.push('\\'),
                Some((_, '"')) => value.push('"'),
                Some((_, other)) => return Err(format!("invalid escape \\{other} in label {key}")),
                None => return Err(unterminated()),
            },
            Some((_, character)) => value.push(character),
            None => return Err(unterminated()),
        }
    }
}

/// Go's `strconv.ParseFloat` for what an exposition holds: decimals, exponents, `NaN` and the
/// signed infinities in any case.
fn parse_value(text: &str) -> Option<f64> {
    let unsigned = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(text);
    if unsigned.eq_ignore_ascii_case("inf") || unsigned.eq_ignore_ascii_case("infinity") {
        return Some(if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if text.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    if !text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Scrape, format_sample, labels};

    const EXPOSITION: &str = r#"# HELP swarm_service_desired_replicas Desired replicas.
# TYPE swarm_service_desired_replicas gauge
swarm_service_desired_replicas{service="web",stack="it",team=""} 2
swarm_service_desired_replicas{service="db",stack="it",team="a \"b\"\\c\nd"} 1
# TYPE swarm_exporter_polls_total counter
swarm_exporter_polls_total 7 1700000000000
# TYPE request_seconds histogram
request_seconds_bucket{le="+Inf"} 3
request_seconds_sum 1.5
request_seconds_count 3
# TYPE rpc summary
rpc{quantile="0.5"} 0.1
rpc_count 4
untyped_thing{a="1"} +Inf
nan_thing NaN
"#;

    fn parsed() -> Scrape {
        Scrape::parse(EXPOSITION).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn reads_gauges_counters_and_untyped_and_skips_the_rest() {
        let scrape = parsed();
        let names: Vec<&str> = scrape
            .samples
            .iter()
            .map(|sample| sample.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "swarm_service_desired_replicas",
                "swarm_service_desired_replicas",
                "swarm_exporter_polls_total",
                "untyped_thing",
                "nan_thing",
            ]
        );
        let db = labels(&[("service", "db")]);
        assert_eq!(
            scrape.value("swarm_service_desired_replicas", &db),
            Some(1.0)
        );
        assert_eq!(
            scrape.find("swarm_service_desired_replicas", &db)[0].labels["team"],
            "a \"b\"\\c\nd",
            "label escapes"
        );
        let none = BTreeMap::new();
        assert!((scrape.sum("swarm_service_desired_replicas", &none) - 3.0).abs() < f64::EPSILON);
        assert_eq!(scrape.value("swarm_service_desired_replicas", &none), None);
        assert!(!scrape.has(
            "swarm_service_desired_replicas",
            &labels(&[("stack", "other")])
        ));
        assert_eq!(scrape.value("swarm_exporter_polls_total", &none), Some(7.0));
        assert_eq!(scrape.value("untyped_thing", &none), Some(f64::INFINITY));
        assert!(scrape.samples[4].value.is_nan());
    }

    #[test]
    fn formats_as_go_quotes_and_floats() {
        let scrape = parsed();
        assert_eq!(
            format_sample(&scrape.samples[1]),
            r#"swarm_service_desired_replicas{service="db",stack="it",team="a \"b\"\\c\nd"} 1"#
        );
        assert_eq!(
            format_sample(&scrape.samples[2]),
            "swarm_exporter_polls_total{} 7"
        );
        assert_eq!(scrape.lines_with("stack", "it").len(), 2);
        let large = Scrape::parse("x 1234567\ny 0.5\n").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(format_sample(&large.samples[0]), "x{} 1.234567e+06");
        assert_eq!(format_sample(&large.samples[1]), "y{} 0.5");
    }

    #[test]
    fn rejects_malformed_lines() {
        for bad in [
            "1x 1",
            "x{a=\"1\" 1",
            "x{a=1} 1",
            "x{a=\"1\",a=\"2\"} 1",
            "x{a=\"\\t\"} 1",
            "x",
            "x one",
            "x 1 soon",
            "x 1 2 3",
            "# TYPE x",
        ] {
            assert!(Scrape::parse(bad).is_err(), "{bad:?} parsed");
        }
    }
}
