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

//! The Go formatters against values Go printed, each file a capture from a throwaway Go program:
//!
//! - `floats/go-corpus.tsv` (S2): bits, `FormatFloat(f, 'g', -1, 64)`, expfmt's `writeFloat` and
//!   a note, for 34 edge values;
//! - `logger/fuzz_numbers.txt` (S4): bits, `FormatFloat 'g'`, `json.Marshal` (or `ERR`), the bits
//!   shifted into an `int64` and its `Duration.String`, for the first 2,000 of S4's 20,000 random values;
//! - `time/go-layouts.tsv` (S7): unix seconds, nanoseconds, the zone offset, `RFC3339Nano` and
//!   `2006-01-02T15:04:05.000Z07:00`, for 70 instant and zone pairs.

use std::fs;
use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::Offset;
use swarm_scheduler_exporter::gofmt_compat::{duration, float, sort, time_layout};

fn testdata(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn bits(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap_or_else(|error| panic!("{hex}: {error}")))
}

/// Compares every line, then fails once with the count and the first mismatches.
fn expect_all(name: &str, mismatches: &[String], checked: usize) {
    assert!(checked > 0, "{name}: no line checked");
    assert!(
        mismatches.is_empty(),
        "{name}: {} of {checked} lines differ from Go, first: {:#?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(10)]
    );
}

fn check_float_file(name: &str) -> usize {
    let data = testdata(name);
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for line in data.lines() {
        let columns: Vec<&str> = line.split('\t').collect();
        let value = bits(columns[0]);
        let got = float::format_g(value);
        if got != columns[1] {
            mismatches.push(format!("{}: Go {} Rust {got}", columns[0], columns[1]));
        }
        checked += 1;
    }
    expect_all(name, &mismatches, checked);
    checked
}

#[test]
fn format_g_matches_go_on_the_s2_corpus() {
    assert_eq!(check_float_file("floats/go-corpus.tsv"), 34);
}

#[test]
fn floats_json_and_durations_match_go_on_2000_values() {
    let data = testdata("logger/fuzz_numbers.txt");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for line in data.lines() {
        let columns: Vec<&str> = line.split(' ').collect();
        assert_eq!(columns.len(), 5, "malformed line {line:?}");
        let value = bits(columns[0]);
        let shortest = float::format_g(value);
        let json = float::format_json(value).unwrap_or_else(|_| "ERR".to_owned());
        let nanos: i64 = columns[3]
            .parse()
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        let duration = duration::format(nanos);
        if (shortest.as_str(), json.as_str(), duration.as_str())
            != (columns[1], columns[2], columns[4])
        {
            mismatches.push(format!("Go {line} Rust {shortest} {json} {duration}"));
        }
        checked += 1;
    }
    expect_all("logger/fuzz_numbers.txt", &mismatches, checked);
    assert_eq!(checked, 2_000);
}

#[test]
fn time_layouts_match_go_on_the_s7_corpus() {
    let data = testdata("time/go-layouts.tsv");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for line in data.lines() {
        let columns: Vec<&str> = line.split('\t').collect();
        let parse = |index: usize| -> i64 {
            columns[index]
                .parse()
                .unwrap_or_else(|error| panic!("{line}: {error}"))
        };
        let timestamp = Timestamp::new(parse(0), i32::try_from(parse(1)).unwrap_or_default())
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        let offset = Offset::from_seconds(i32::try_from(parse(2)).unwrap_or_default())
            .unwrap_or_else(|error| panic!("{line}: {error}"));
        let nano = time_layout::format_rfc3339_nano(timestamp, offset);
        let mut millis = String::new();
        time_layout::push_rfc3339_millis(&mut millis, timestamp, offset);
        if (nano.as_str(), millis.as_str()) != (columns[3], columns[4]) {
            mismatches.push(format!("Go {line} Rust {nano} {millis}"));
        }
        checked += 1;
    }
    expect_all("time/go-layouts.tsv", &mismatches, checked);
    assert_eq!(checked, 70);
}

/// goautoneg's `Accept` comparison, which is not a strict weak order: a clause is less than
/// another if its q is higher, or its type or subtype is concrete where the other's is `*`.
fn autoneg_less(left: &(usize, &str), right: &(usize, &str)) -> bool {
    let (left, right) = (left.1.as_bytes(), right.1.as_bytes());
    let quality = |code: u8| match code {
        b'a' => 1.0,
        b'b' => 0.5,
        _ => 0.1,
    };
    quality(left[0]) > quality(right[0])
        || (left[1] != b'*' && right[1] == b'*')
        || (left[2] != b'*' && right[2] == b'*')
}

#[test]
fn sort_matches_go_on_237_inputs() {
    let data = testdata("sort/go-sort.tsv");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    let mut differs_from_insertion = 0;
    for line in data.lines() {
        let (codes, order) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("malformed line {line:?}"));
        let mut items: Vec<(usize, &str)> = codes.split(' ').enumerate().collect();
        let mut insertion = items.clone();
        sort::sort(&mut items, autoneg_less);
        let got: Vec<String> = items.iter().map(|(index, _)| index.to_string()).collect();
        if got.join(" ") != order {
            mismatches.push(format!("{codes}: Go {order} Rust {}", got.join(" ")));
        }
        // The control: a plain insertion sort, Go's algorithm up to 12 elements, orders many of the
        // longer inputs differently. (Rust's own sort refuses the comparison: it panics with
        // "user-provided comparison function does not correctly implement a total order".)
        for outer in 1..insertion.len() {
            let mut inner = outer;
            while inner > 0 && autoneg_less(&insertion[inner], &insertion[inner - 1]) {
                insertion.swap(inner, inner - 1);
                inner -= 1;
            }
        }
        if insertion != items {
            differs_from_insertion += 1;
        }
        checked += 1;
    }
    expect_all("sort/go-sort.tsv", &mismatches, checked);
    assert_eq!(checked, 237);
    assert!(
        differs_from_insertion >= 200,
        "only {differs_from_insertion} inputs tell pdqsort from insertion sort"
    );
}
