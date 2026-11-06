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

//! A reader of the exposition text, for tests: the families' types and help, and every sample
//! with its labels and value. Shared by the unit tests and the integration tests (which include
//! this file), so it names nothing of the crate.

use std::collections::BTreeMap;

/// One sample line: `name{labels} value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// The sample name, a counter's with its `_total`, a histogram's with its suffix.
    pub name: String,
    /// The labels, in the order written.
    pub labels: Vec<(String, String)>,
    /// The value.
    pub value: f64,
}

/// A parsed exposition.
#[derive(Debug, Clone, Default)]
pub struct Exposition {
    /// The family name to its `# TYPE`.
    pub types: BTreeMap<String, String>,
    /// The family name to its `# HELP`.
    pub help: BTreeMap<String, String>,
    /// Every sample, in order.
    pub samples: Vec<Sample>,
    /// Whether the text ended with `# EOF`.
    pub eof: bool,
}

/// `name{a="x",b="y"}`, the label names sorted, the values escaped.
#[must_use]
pub fn series_id(name: &str, labels: &[(&str, &str)]) -> String {
    let mut labels = labels.to_vec();
    labels.sort_unstable();
    let pairs: Vec<String> = labels
        .iter()
        .map(|(label, value)| format!("{label}={value:?}"))
        .collect();
    format!("{name}{{{}}}", pairs.join(","))
}

/// Parses `text`.
///
/// # Panics
///
/// A line that is neither a comment nor a sample.
#[must_use]
pub fn parse(text: &str) -> Exposition {
    let mut parsed = Exposition::default();
    for line in text.lines() {
        if line == "# EOF" {
            parsed.eof = true;
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            let Some((name, kind)) = rest.split_once(' ') else {
                panic!("a TYPE line has a type: {line:?}");
            };
            parsed.types.insert(name.to_owned(), kind.to_owned());
        } else if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, help) = rest.split_once(' ').unwrap_or((rest, ""));
            parsed.help.insert(name.to_owned(), help.to_owned());
        } else if !line.starts_with('#') && !line.is_empty() {
            parsed.samples.push(parse_sample(line));
        }
    }
    parsed
}

fn parse_sample(line: &str) -> Sample {
    let (name, rest) = match line.find(['{', ' ']) {
        Some(index) => line.split_at(index),
        None => panic!("not a sample: {line:?}"),
    };
    let (labels, value) = if let Some(inner) = rest.strip_prefix('{') {
        parse_labels(inner, line)
    } else {
        (Vec::new(), rest)
    };
    let value = value.trim();
    // A sample may carry a timestamp after its value; none of ours does.
    let value = value.split(' ').next().unwrap_or(value);
    Sample {
        name: name.to_owned(),
        labels,
        value: match value {
            "+Inf" | "Inf" => f64::INFINITY,
            "-Inf" => f64::NEG_INFINITY,
            "NaN" => f64::NAN,
            number => number
                .parse()
                .unwrap_or_else(|error| panic!("bad value in {line:?}: {error}")),
        },
    }
}

/// The labels of `inner` (what follows `{`) and the rest of the line after the closing `}`.
fn parse_labels<'a>(inner: &'a str, line: &str) -> (Vec<(String, String)>, &'a str) {
    let mut labels = Vec::new();
    let mut chars = inner.char_indices().peekable();
    loop {
        // A name up to `=`, or the closing brace.
        let Some(&(start, first)) = chars.peek() else {
            panic!("unterminated labels in {line:?}");
        };
        if first == '}' {
            return (labels, &inner[start + 1..]);
        }
        let mut name_end = start;
        while let Some(&(index, current)) = chars.peek() {
            if current == '=' {
                name_end = index;
                chars.next();
                break;
            }
            chars.next();
        }
        let name = inner[start..name_end].to_owned();
        assert_eq!(chars.next().map(|(_, quote)| quote), Some('"'), "{line:?}");
        let mut value = String::new();
        loop {
            match chars.next() {
                Some((_, '"')) => break,
                Some((_, '\\')) => match chars.next() {
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, other)) => value.push(other),
                    None => panic!("dangling escape in {line:?}"),
                },
                Some((_, other)) => value.push(other),
                None => panic!("unterminated value in {line:?}"),
            }
        }
        labels.push((name, value));
        if let Some(&(_, ',')) = chars.peek() {
            chars.next();
        }
    }
}

impl Exposition {
    /// The scalar series by [`series_id`].
    #[must_use]
    pub fn series(&self) -> BTreeMap<String, f64> {
        self.samples
            .iter()
            .map(|sample| {
                let labels: Vec<(&str, &str)> = sample
                    .labels
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();
                (series_id(&sample.name, &labels), sample.value)
            })
            .collect()
    }

    /// The value of the sample `name` with exactly `labels`, if any.
    #[must_use]
    pub fn value(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        self.series().get(&series_id(name, labels)).copied()
    }
}
