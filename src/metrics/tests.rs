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

//! Tests of the metric building blocks.

use std::collections::BTreeMap;

use super::*;
use crate::test_support::openmetrics::{parse, series_id};

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn exposition(collector: &Arc<impl Collector>) -> crate::test_support::openmetrics::Exposition {
    let mut registry = Registry::default();
    register(&mut registry, collector);
    parse(&render(&registry))
}

#[test]
fn a_gauge_vec_keeps_each_series_until_it_is_deleted() {
    let vec = Arc::new(GaugeVec::new(Desc::new("m", "help.", &["b", "a"])));
    let one = labels(&[("a", "1"), ("b", "2")]);
    vec.set_with(&one, 5.0).unwrap();
    vec.set(&["4", "3"], 7.0).unwrap();
    assert_eq!(vec.get_with(&one), Some(5.0));
    let text = exposition(&vec);
    assert_eq!(text.types["m"], "gauge");
    assert_eq!(text.help["m"], "help.");
    assert_eq!(
        text.series(),
        BTreeMap::from([
            (series_id("m", &[("a", "1"), ("b", "2")]), 5.0),
            (series_id("m", &[("a", "3"), ("b", "4")]), 7.0),
        ])
    );
    assert!(vec.delete_with(&one));
    assert!(!vec.delete_with(&one), "already deleted");
    assert_eq!(vec.get_with(&one), None);
    assert_eq!(exposition(&vec).samples.len(), 1);
}

#[test]
fn an_empty_gauge_vec_encodes_no_family() {
    let vec = Arc::new(GaugeVec::new(Desc::new("m", "help.", &["a"])));
    let text = exposition(&vec);
    assert!(text.types.is_empty() && text.samples.is_empty());
}

#[test]
fn a_label_set_that_is_not_the_familys_is_refused() {
    let vec = GaugeVec::new(Desc::new("m", "help.", &["a", "b"]));
    let wrong = labels(&[("a", "1"), ("c", "2")]);
    let error = vec.set_with(&wrong, 1.0).unwrap_err();
    assert_eq!(
        error.to_string(),
        "label name \"b\" missing in label map for m"
    );
    assert_eq!(
        vec.set_with(&labels(&[("a", "1")]), 1.0)
            .unwrap_err()
            .to_string(),
        "inconsistent label cardinality for m: expected 2 label values but got 1"
    );
    assert!(vec.set(&["1"], 1.0).is_err());
    assert!(!vec.delete_with(&wrong));
}

#[test]
fn a_snapshot_is_replaced_as_a_whole() {
    let desc = Desc::new("m", "help.", &["a"]);
    let family = Arc::new(SnapshotFamily::new(Arc::clone(&desc)));
    assert!(
        exposition(&family).samples.is_empty(),
        "nothing published yet"
    );
    let build = |values: &[(&str, f64)]| {
        let mut builder = SnapshotBuilder::new();
        for &(label, value) in values {
            builder.set(&desc, &[("a", label)], value);
        }
        builder.build().unwrap()
    };
    family.collector().publish(build(&[("x", 1.0), ("y", 2.0)]));
    let before = family.collector().load().unwrap();
    family.collector().publish(build(&[("z", 3.0)]));
    assert_eq!(
        exposition(&family).series(),
        BTreeMap::from([(series_id("m", &[("a", "z")]), 3.0)]),
        "x and y are gone"
    );
    assert_eq!(
        before[0].series().count(),
        2,
        "a loaded snapshot does not change"
    );
}

#[test]
fn a_build_with_a_wrong_label_set_publishes_nothing() {
    let desc = Desc::new("m", "help.", &["a", "b"]);
    let mut builder = SnapshotBuilder::new();
    builder.set(&desc, &[("a", "1"), ("b", "2")], 1.0);
    builder.set(&desc, &[("a", "1"), ("c", "2")], 1.0);
    builder.set(&desc, &[("a", "1")], 1.0);
    assert_eq!(
        builder.build().unwrap_err().to_string(),
        "label set does not match the family's label names: missing \"b\", want [a b]"
    );
}

#[test]
fn setting_a_label_set_twice_keeps_the_last_value() {
    let desc = Desc::new("m", "help.", &["a"]);
    let mut builder = SnapshotBuilder::new();
    builder.set(&desc, &[("a", "1")], 1.0);
    builder.set(&desc, &[("a", "1")], 9.0);
    let families = builder.build().unwrap();
    assert_eq!(
        families[0]
            .series()
            .map(|(_, value)| value)
            .collect::<Vec<_>>(),
        [9.0]
    );
}

#[test]
fn a_scrape_gauge_reads_its_value_at_every_scrape() {
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let reader = Arc::clone(&counter);
    let gauge = ScrapeGauge::new("g", "help.", move || {
        f64::from(reader.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    });
    assert_eq!(exposition(&gauge).value("g", &[]), Some(0.0));
    assert_eq!(exposition(&gauge).value("g", &[]), Some(1.0));
}

#[test]
fn label_values_are_escaped() {
    let vec = Arc::new(GaugeVec::new(Desc::new("m", "help.", &["a"])));
    vec.set(&["quote\" back\\slash\nnewline"], 1.0).unwrap();
    let text = exposition(&vec);
    assert_eq!(text.samples[0].labels[0].1, "quote\" back\\slash\nnewline");
}

#[test]
fn the_exposition_is_openmetrics_ending_with_eof() {
    let registry = Registry::default();
    assert_eq!(render(&registry), "# EOF\n");
}
