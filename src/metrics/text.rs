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

//! The text exposition format 0.0.4, a port of expfmt's `MetricFamilyToText` (common v0.71.0
//! `text_create.go`) for counters, gauges and histograms.

use crate::gofmt_compat::float;

use super::{Gathered, Kind, Value};

/// The text format's `Content-Type`, before the `escaping` parameter.
pub const TEXT_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Appends `families`, already gathered (so sorted, and none empty), in the text format.
pub fn encode(out: &mut String, families: &[Gathered]) {
    for family in families {
        encode_family(out, family);
    }
}

fn encode_family(out: &mut String, family: &Gathered) {
    out.push_str("# HELP ");
    push_name(out, &family.name);
    out.push(' ');
    push_escaped(out, &family.help, false);
    out.push('\n');
    out.push_str("# TYPE ");
    push_name(out, &family.name);
    out.push_str(match family.kind {
        Kind::Counter => " counter\n",
        Kind::Gauge => " gauge\n",
        Kind::Histogram => " histogram\n",
    });
    for series in &family.series {
        match &series.value {
            Value::Scalar(value) => {
                push_sample(out, &family.name, "", &series.labels, None, *value);
            }
            Value::Histogram {
                buckets,
                sum,
                count,
            } => {
                let mut infinity_seen = false;
                for &(bound, cumulative) in buckets {
                    push_sample(
                        out,
                        &family.name,
                        "_bucket",
                        &series.labels,
                        Some(bound),
                        float::from_int(cumulative),
                    );
                    infinity_seen |= bound == f64::INFINITY;
                }
                if !infinity_seen {
                    push_sample(
                        out,
                        &family.name,
                        "_bucket",
                        &series.labels,
                        Some(f64::INFINITY),
                        float::from_int(*count),
                    );
                }
                push_sample(out, &family.name, "_sum", &series.labels, None, *sum);
                push_sample(
                    out,
                    &family.name,
                    "_count",
                    &series.labels,
                    None,
                    float::from_int(*count),
                );
            }
        }
    }
}

/// `writeSample`: the name and suffix, the label pairs plus an optional `le`, and the value.
fn push_sample(
    out: &mut String,
    name: &str,
    suffix: &str,
    labels: &[(String, String)],
    le: Option<f64>,
    value: f64,
) {
    let full_name = format!("{name}{suffix}");
    push_name_and_labels(out, &full_name, labels, le);
    out.push(' ');
    push_float(out, value);
    out.push('\n');
}

/// `writeNameAndLabelPairs`: a name that is not legacy-valid goes inside the braces, quoted.
fn push_name_and_labels(
    out: &mut String,
    name: &str,
    labels: &[(String, String)],
    le: Option<f64>,
) {
    let mut separator = '{';
    let inside_braces = !is_valid_metric_name(name);
    if inside_braces {
        out.push(separator);
        separator = ',';
    }
    push_name(out, name);
    if labels.is_empty() && le.is_none() {
        if inside_braces {
            out.push('}');
        }
        return;
    }
    for (label, value) in labels {
        out.push(separator);
        push_name(out, label);
        out.push_str("=\"");
        push_escaped(out, value, true);
        out.push('"');
        separator = ',';
    }
    if let Some(bound) = le {
        out.push(separator);
        out.push_str("le=\"");
        push_float(out, bound);
        out.push('"');
    }
    out.push('}');
}

/// `writeEscapedString`: `\` as `\\`, a newline as `\n`, and with `quote` a `"` as `\"`.
fn push_escaped(out: &mut String, text: &str, quote: bool) {
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' if quote => out.push_str("\\\""),
            other => out.push(other),
        }
    }
}

/// `writeFloat`: 1, 0 (negative zero included), -1, NaN and the infinities by hand, anything
/// else as `strconv.FormatFloat(f, 'g', -1, 64)`.
pub fn push_float(out: &mut String, value: f64) {
    match value {
        1.0 => out.push('1'),
        // Negative zero too: a float pattern compares as `==` does.
        0.0 => out.push('0'),
        -1.0 => out.push_str("-1"),
        f64::INFINITY => out.push_str("+Inf"),
        f64::NEG_INFINITY => out.push_str("-Inf"),
        _ if value.is_nan() => out.push_str("NaN"),
        _ => float::push_g(out, value),
    }
}

/// `writeName`: as is when legacy-valid, quoted and escaped otherwise.
fn push_name(out: &mut String, name: &str) {
    if is_valid_metric_name(name) {
        out.push_str(name);
    } else {
        out.push('"');
        push_escaped(out, name, true);
        out.push('"');
    }
}

/// model's legacy metric name rule: `[a-zA-Z_:][a-zA-Z0-9_:]*`.
#[must_use]
pub fn is_valid_metric_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_' || first == b':')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':')
}
