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

use std::collections::BTreeMap;
use std::sync::Arc;

use super::testing::{gather_series, series_id};
use super::text::{self, push_float};
use super::{
    Collector, Counter, CounterVec, DEF_BUCKETS, Desc, Gauge, GaugeFunc, GaugeVec, Histogram, Kind,
    Registry, SnapshotBuilder, SnapshotCollector, SnapshotFamily, Value, snapshot,
};

const TEST_SNAPSHOT_FAMILY: &str = "test_snapshot";
const TEST_SNAPSHOT_OTHER_FAMILY: &str = "test_snapshot_other";
const TEST_SNAPSHOT_LABEL_NAMES: [&str; 2] = ["first", "second"];

fn new_test_snapshot_desc(name: &str) -> Arc<Desc> {
    Desc::new(name, "test", Kind::Gauge, &TEST_SNAPSHOT_LABEL_NAMES).expect("desc")
}

fn test_snapshot_labels<'a>(first: &'a str, second: &'a str) -> [(&'static str, &'a str); 2] {
    [("first", first), ("second", second)]
}

/// Builds `series` (first label value to value, the second label fixed at `x`) for `desc`.
fn build_test_snapshot(desc: &Arc<Desc>, series: &[(&str, f64)]) -> Vec<super::Family> {
    let mut builder = SnapshotBuilder::new();
    for &(first, value) in series {
        builder.set(desc, &test_snapshot_labels(first, "x"), value);
    }
    builder.build().expect("build")
}

/// What [`gather_series`] returns for a snapshot built by [`build_test_snapshot`].
fn expected_test_series(name: &str, series: &[(&str, f64)]) -> BTreeMap<String, f64> {
    series
        .iter()
        .map(|&(first, value)| (series_id(name, &test_snapshot_labels(first, "x")), value))
        .collect()
}

fn snapshot_collector(descs: &[&Arc<Desc>]) -> Arc<SnapshotCollector> {
    Arc::new(SnapshotCollector::new(
        descs.iter().map(|&desc| Arc::clone(desc)).collect(),
    ))
}

fn gathered(collector: &Arc<SnapshotCollector>) -> BTreeMap<String, f64> {
    gather_series(Arc::clone(collector) as Arc<dyn Collector>)
}

/// Go: `TestSnapshotCollector_NothingPublished_EmitsNoFamily`
#[test]
fn snapshot_nothing_published_emits_no_family() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let collector = snapshot_collector(&[&desc]);
    assert_eq!(gathered(&collector), BTreeMap::new());
    collector.publish(build_test_snapshot(&desc, &[]));
    assert_eq!(gathered(&collector), BTreeMap::new());
}

/// Go: `TestSnapshotCollector_PublishDuringCollect_ScrapeKeepsItsSnapshot`. Go blocks `Collect`
/// mid-send on an unbuffered channel; here a scrape loads the published set once, so a publish
/// after that load is the same interleaving: the scrape still writes all of snapshot A.
#[test]
fn snapshot_publish_during_collect_scrape_keeps_its_snapshot() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let collector = snapshot_collector(&[&desc]);
    let series_a = [("a1", 1.0), ("a2", 2.0), ("a3", 3.0)];
    let series_b = [("b1", 10.0), ("b2", 20.0)];
    let families_a = build_test_snapshot(&desc, &series_a);
    collector.publish(families_a.clone());

    let loaded = collector.load().expect("snapshot A is published");
    collector.publish(build_test_snapshot(&desc, &series_b));
    assert_eq!(*loaded, families_a);

    assert_eq!(
        gathered(&collector),
        expected_test_series(TEST_SNAPSHOT_FAMILY, &series_b)
    );
}

/// Go: `TestSnapshotCollector_FamiliesPublishedTogether`
#[test]
fn snapshot_families_published_together() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let other_desc = new_test_snapshot_desc(TEST_SNAPSHOT_OTHER_FAMILY);
    let collector = snapshot_collector(&[&desc, &other_desc]);
    let mut builder = SnapshotBuilder::new();
    builder.set(&desc, &test_snapshot_labels("a", "b"), 1.0);
    builder.set(&other_desc, &test_snapshot_labels("a", "b"), 2.0);
    collector.publish(builder.build().expect("build"));
    assert_eq!(
        gathered(&collector),
        BTreeMap::from([
            (
                series_id(TEST_SNAPSHOT_FAMILY, &test_snapshot_labels("a", "b")),
                1.0
            ),
            (
                series_id(TEST_SNAPSHOT_OTHER_FAMILY, &test_snapshot_labels("a", "b")),
                2.0
            ),
        ])
    );
}

/// Go: `TestSeriesKey_DistinctValueListsNeverCollide`
#[test]
fn series_key_distinct_value_lists_never_collide() {
    let pairs: [(&str, [&str; 2], [&str; 2]); 3] = [
        ("separator inside a value", ["a|b", "c"], ["a", "b|c"]),
        ("length prefix inside a value", ["1:a", ""], ["", "1:a"]),
        ("empty values", ["", "ab"], ["ab", ""]),
    ];
    for (name, left, right) in pairs {
        let left: Vec<String> = left.iter().map(|&value| value.to_owned()).collect();
        let right: Vec<String> = right.iter().map(|&value| value.to_owned()).collect();
        assert_ne!(
            snapshot::series_key(&left),
            snapshot::series_key(&right),
            "{name}"
        );
    }
}

/// Go: `TestSnapshotBuilder_SeparatorInValues_StaysTwoSeries`
#[test]
fn snapshot_separator_in_values_stays_two_series() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let collector = snapshot_collector(&[&desc]);
    let mut builder = SnapshotBuilder::new();
    builder.set(&desc, &test_snapshot_labels("a|b", "c"), 1.0);
    builder.set(&desc, &test_snapshot_labels("a", "b|c"), 2.0);
    collector.publish(builder.build().expect("build"));
    assert_eq!(
        gathered(&collector),
        BTreeMap::from([
            (
                series_id(TEST_SNAPSHOT_FAMILY, &test_snapshot_labels("a|b", "c")),
                1.0
            ),
            (
                series_id(TEST_SNAPSHOT_FAMILY, &test_snapshot_labels("a", "b|c")),
                2.0
            ),
        ])
    );
}

/// Go: `TestSnapshotBuilder_DuplicateLabelSet_LastWriteWins`
#[test]
fn snapshot_duplicate_label_set_last_write_wins() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let collector = snapshot_collector(&[&desc]);
    let mut builder = SnapshotBuilder::new();
    builder.set(&desc, &test_snapshot_labels("a", "b"), 1.0);
    builder.set(&desc, &test_snapshot_labels("a", "b"), 2.0);
    let families = builder.build().expect("build");
    assert_eq!(families.len(), 1);
    assert_eq!(families[0].series.len(), 1);
    collector.publish(families);
    assert_eq!(
        gathered(&collector).get(&series_id(
            TEST_SNAPSHOT_FAMILY,
            &test_snapshot_labels("a", "b")
        )),
        Some(&2.0)
    );
}

/// A build error case: its name, the label set, and the error.
type BuildErrorCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

/// Go: `TestSnapshotBuilder_BuildError_KeepsPreviousSnapshot`, the missing and extra label
/// cases. Its invalid UTF-8 case has no Rust counterpart: a label value is a `&str`, which cannot
/// hold invalid UTF-8, so the input cannot be built.
#[test]
fn snapshot_build_error_keeps_previous_snapshot() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let cases: [BuildErrorCase<'_>; 3] = [
        (
            "missing label",
            &[("first", "a"), ("other", "b")],
            "label set does not match the family's label names: missing \"second\", want [first \
             second]",
        ),
        (
            "extra label",
            &[("first", "a"), ("second", "b"), ("third", "c")],
            "label set does not match the family's label names: got 3 labels, want [first second]",
        ),
        (
            "a label given twice counts once",
            &[("first", "a"), ("first", "b")],
            "label set does not match the family's label names: got 1 labels, want [first second]",
        ),
    ];
    for (name, labels, want_error) in cases {
        let collector = snapshot_collector(&[&desc]);
        let series_a = [("a1", 1.0), ("a2", 2.0)];
        collector.publish(build_test_snapshot(&desc, &series_a));

        let mut builder = SnapshotBuilder::new();
        builder.set(&desc, &test_snapshot_labels("ok", "b"), 5.0);
        builder.set(&desc, labels, 6.0);
        // A later error does not replace the first.
        builder.set(&desc, &[("only", "one")], 7.0);
        let error = builder.build().expect_err(name);
        assert_eq!(error.to_string(), want_error, "{name}");

        // The update flow publishes only on success, so the previous snapshot stays.
        assert_eq!(
            gathered(&collector),
            expected_test_series(TEST_SNAPSHOT_FAMILY, &series_a),
            "{name}"
        );
    }
}

/// A single-family snapshot publishes and gathers like the collector it wraps.
#[test]
fn snapshot_family_wraps_one_descriptor() {
    let desc = new_test_snapshot_desc(TEST_SNAPSHOT_FAMILY);
    let family = Arc::new(SnapshotFamily::new(Arc::clone(&desc)));
    assert_eq!(family.desc(), &desc);
    assert_eq!(family.descs(), vec![Arc::clone(&desc)]);
    family
        .collector()
        .publish(build_test_snapshot(&desc, &[("a", 1.0)]));
    assert_eq!(
        gather_series(family),
        expected_test_series(TEST_SNAPSHOT_FAMILY, &[("a", 1.0)])
    );
}

/// The registration mechanism the Go tests `TestConfigure*_RegistersWithoutPanic` exercise
/// (the metrics registration tests); each named test lands with its collector. Go's `MustRegister`
/// panics on a second registration of a name; `register` returns an error instead, which no
/// caller can reach because each family is registered once at startup, so the difference is not
/// observable.
#[test]
fn registry_registers_each_name_once() {
    let registry = Registry::new();
    let gauge = Arc::new(Gauge::new(
        Desc::new("a_gauge", "A gauge.", Kind::Gauge, &[]).expect("desc"),
    ));
    registry
        .register(Arc::clone(&gauge) as Arc<dyn Collector>)
        .expect("first registration");
    let error = registry
        .register(Arc::clone(&gauge) as Arc<dyn Collector>)
        .expect_err("second registration");
    assert_eq!(
        error.to_string(),
        "duplicate metrics collector registration attempted: \"a_gauge\" is already registered"
    );
    // Another collector with one name taken is refused whole.
    let snapshot = Arc::new(SnapshotCollector::new(vec![
        Desc::new("b_gauge", "B.", Kind::Gauge, &[]).expect("desc"),
        Desc::new("a_gauge", "A again.", Kind::Gauge, &[]).expect("desc"),
    ]));
    assert!(registry.register(snapshot).is_err());
    let other = Arc::new(Gauge::new(
        Desc::new("b_gauge", "B.", Kind::Gauge, &[]).expect("desc"),
    ));
    registry
        .register(other)
        .expect("b_gauge was not registered by the refused collector");
}

/// The Prometheus Go client's `NewDesc` and `NewHistogram` checks, as errors.
#[test]
fn desc_refuses_what_the_go_client_refuses() {
    let cases: [(&str, Kind, &[&str], &str); 6] = [
        (
            "1abc",
            Kind::Gauge,
            &[],
            "\"1abc\" is not a valid metric name",
        ),
        (
            "a-b",
            Kind::Gauge,
            &[],
            "\"a-b\" is not a valid metric name",
        ),
        (
            "abc",
            Kind::Gauge,
            &["__x"],
            "\"__x\" is not a valid label name for metric \"abc\"",
        ),
        (
            "abc",
            Kind::Gauge,
            &["a:b"],
            "\"a:b\" is not a valid label name for metric \"abc\"",
        ),
        (
            "abc",
            Kind::Gauge,
            &["x", "x"],
            "duplicate label names in constant and variable labels for metric \"abc\"",
        ),
        (
            "abc",
            Kind::Histogram,
            &["le"],
            "\"le\" is not allowed as label name in histograms (metric \"abc\")",
        ),
    ];
    for (name, kind, labels, want) in cases {
        assert_eq!(
            Desc::new(name, "h", kind, labels)
                .expect_err(name)
                .to_string(),
            want
        );
    }
    assert!(Desc::new("a:b_c", "h", Kind::Gauge, &["le", "_x"]).is_ok());
}

/// The family types behave as the Prometheus Go client's: a vec creates a child on first use and
/// deletes it, a counter ignores a negative add (the Prometheus Go client panics there; the
/// exporter never adds a negative), a histogram counts an observation in the first bucket at or
/// above it, and a gauge func is evaluated at every gather.
#[test]
fn families_behave_as_the_go_client() {
    let vec = GaugeVec::new(Desc::new("v", "h", Kind::Gauge, &["a"]).expect("desc"));
    vec.set(&["x"], 2.0).expect("set");
    assert_eq!(vec.get(&["x"]), Some(2.0));
    assert!(vec.set(&["x", "y"], 1.0).is_err());
    assert!(vec.delete(&["x"]));
    assert!(!vec.delete(&["x"]));
    assert_eq!(vec.get(&["x"]), None);

    let gauge = Gauge::new(Desc::new("g", "h", Kind::Gauge, &[]).expect("desc"));
    gauge.set(1.5);
    gauge.add(-0.5);
    assert!((gauge.get() - 1.0).abs() < f64::EPSILON);

    let counter = Counter::new(Desc::new("c", "h", Kind::Counter, &[]).expect("desc"));
    counter.inc();
    counter.add(2.0);
    counter.add(-5.0);
    assert!((counter.get() - 3.0).abs() < f64::EPSILON);

    let counters = CounterVec::new(Desc::new("cv", "h", Kind::Counter, &["a"]).expect("desc"));
    counters.add(&["x"], 0.0).expect("add");
    assert_eq!(counters.get(&["x"]), Some(0.0));
    counters.inc(&["x"]).expect("inc");
    assert_eq!(counters.get(&["x"]), Some(1.0));
    assert!(counters.inc(&[]).is_err());

    let histogram = Histogram::new(
        Desc::new("h", "h", Kind::Histogram, &[]).expect("desc"),
        &[1.0, 2.0, f64::INFINITY],
    );
    for value in [0.5, 1.0, 1.5, 3.0, f64::NAN] {
        histogram.observe(value);
    }
    let mut families = Vec::new();
    histogram.collect(&mut families);
    let Value::Histogram {
        buckets,
        sum,
        count,
    } = &families[0].series[0].value
    else {
        panic!("not a histogram");
    };
    assert_eq!(buckets, &[(1.0, 2), (2.0, 3)]);
    assert!(sum.is_nan());
    assert_eq!(*count, 5);

    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let in_func = Arc::clone(&calls);
    let func = Arc::new(GaugeFunc::new(
        Desc::new("f", "h", Kind::Gauge, &[]).expect("desc"),
        move || f64::from(in_func.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
    ));
    let registry = Registry::new();
    registry.register(func).expect("register");
    assert_eq!(registry.gather()[0].series[0].value, Value::Scalar(0.0));
    assert_eq!(registry.gather()[0].series[0].value, Value::Scalar(1.0));
}

/// S2's family set (`gotools/cmd/families`), encoded byte for byte as the Prometheus Go client
/// v1.24.1 and expfmt (common v0.71.0) wrote it: families by name, the empty vec dropped, label
/// pairs by name whatever their declared order, series by label values, help and label escaping,
/// and the histogram with `le` written by the float formatter.
#[test]
fn encoding_matches_go_on_the_s2_family_set() {
    let registry = Registry::new();
    let states = Arc::new(GaugeVec::new(
        Desc::new(
            "zz_state",
            "Help with \\ backslash and\nnewline and \"quotes\".",
            Kind::Gauge,
            &["zone", "name"],
        )
        .expect("desc"),
    ));
    let counter = Arc::new(Counter::new(
        Desc::new("aa_events_total", "A plain counter.", Kind::Counter, &[]).expect("desc"),
    ));
    let requests = Arc::new(CounterVec::new(
        Desc::new(
            "mm_requests_total",
            "Requests by code.",
            Kind::Counter,
            &["method", "code"],
        )
        .expect("desc"),
    ));
    let histogram = Arc::new(Histogram::new(
        Desc::new("hh_duration_seconds", "Durations.", Kind::Histogram, &[]).expect("desc"),
        &DEF_BUCKETS,
    ));
    let timestamp = Arc::new(Gauge::new(
        Desc::new("tt_timestamp_seconds", "Unix time.", Kind::Gauge, &[]).expect("desc"),
    ));
    let empty = Arc::new(GaugeVec::new(
        Desc::new("ee_empty", "Vec with no children.", Kind::Gauge, &["x"]).expect("desc"),
    ));
    let collectors: [Arc<dyn Collector>; 6] = [
        Arc::clone(&states) as Arc<dyn Collector>,
        Arc::clone(&counter) as Arc<dyn Collector>,
        Arc::clone(&requests) as Arc<dyn Collector>,
        Arc::clone(&histogram) as Arc<dyn Collector>,
        Arc::clone(&timestamp) as Arc<dyn Collector>,
        empty,
    ];
    for collector in collectors {
        registry.register(collector).expect("register");
    }

    states.set(&["z2", "b"], 1.0).expect("set");
    states.set(&["z1", "a\"b\\c\nd"], 0.5).expect("set");
    states.set(&["z1", "b"], 1_234_567.0).expect("set");
    states.set(&["z1", "a"], -1.0).expect("set");
    states.set(&["", "a"], 0.0).expect("set");
    states.set(&["z1", "ä ünïcode"], 1e-5).expect("set");
    counter.add(42.0);
    requests.inc(&["POST", "500"]).expect("inc");
    requests.add(&["GET", "200"], 10.0).expect("add");
    requests.add(&["GET", "500"], 2.0).expect("add");
    requests.add(&["GET", "1000"], 3.0).expect("add");
    for value in [0.003, 0.7, 12.0, 0.005] {
        histogram.observe(value);
    }
    timestamp.set(1.729_771_234_123_456e9);

    let mut out = String::new();
    text::encode(&mut out, &registry.gather());
    assert_eq!(out, GO_FAMILIES);
}

/// S2's `families/go.txt`.
const GO_FAMILIES: &str = r#"# HELP aa_events_total A plain counter.
# TYPE aa_events_total counter
aa_events_total 42
# HELP hh_duration_seconds Durations.
# TYPE hh_duration_seconds histogram
hh_duration_seconds_bucket{le="0.005"} 2
hh_duration_seconds_bucket{le="0.01"} 2
hh_duration_seconds_bucket{le="0.025"} 2
hh_duration_seconds_bucket{le="0.05"} 2
hh_duration_seconds_bucket{le="0.1"} 2
hh_duration_seconds_bucket{le="0.25"} 2
hh_duration_seconds_bucket{le="0.5"} 2
hh_duration_seconds_bucket{le="1"} 3
hh_duration_seconds_bucket{le="2.5"} 3
hh_duration_seconds_bucket{le="5"} 3
hh_duration_seconds_bucket{le="10"} 3
hh_duration_seconds_bucket{le="+Inf"} 4
hh_duration_seconds_sum 12.708
hh_duration_seconds_count 4
# HELP mm_requests_total Requests by code.
# TYPE mm_requests_total counter
mm_requests_total{code="1000",method="GET"} 3
mm_requests_total{code="200",method="GET"} 10
mm_requests_total{code="500",method="GET"} 2
mm_requests_total{code="500",method="POST"} 1
# HELP tt_timestamp_seconds Unix time.
# TYPE tt_timestamp_seconds gauge
tt_timestamp_seconds 1.729771234123456e+09
# HELP zz_state Help with \\ backslash and\nnewline and "quotes".
# TYPE zz_state gauge
zz_state{name="a",zone=""} 0
zz_state{name="a",zone="z1"} -1
zz_state{name="a\"b\\c\nd",zone="z1"} 0.5
zz_state{name="b",zone="z1"} 1.234567e+06
zz_state{name="b",zone="z2"} 1
zz_state{name="ä ünïcode",zone="z1"} 1e-05
"#;

/// Series of different label counts (only possible across a snapshot's families, never within one)
/// sort shorter first, as the Prometheus Go client's `metricSorter` does; a family with no series
/// is dropped.
#[test]
fn gather_sorts_series_by_label_values() {
    let desc = Desc::new("s", "h", Kind::Gauge, &["b", "a"]).expect("desc");
    let collector = SnapshotCollector::new(vec![Arc::clone(&desc)]);
    let mut builder = SnapshotBuilder::new();
    for (second, first) in [("2", "1"), ("1", "2"), ("1", "10"), ("0", "1")] {
        builder.set(&desc, &[("b", second), ("a", first)], 1.0);
    }
    collector.publish(builder.build().expect("build"));
    let registry = Registry::new();
    registry.register(Arc::new(collector)).expect("register");
    let order: Vec<Vec<(String, String)>> = registry.gather()[0]
        .series
        .iter()
        .map(|series| series.labels.clone())
        .collect();
    let pairs = |first: &str, second: &str| {
        vec![
            ("a".to_owned(), first.to_owned()),
            ("b".to_owned(), second.to_owned()),
        ]
    };
    assert_eq!(
        order,
        vec![
            pairs("1", "0"),
            pairs("1", "2"),
            pairs("10", "1"),
            pairs("2", "1")
        ]
    );
}

/// expfmt's `writeFloat` on S2's 34 edge values (`floats/go-corpus.tsv`, column 3, as Go
/// printed them): the bits, Go's text, and the input as the Go program wrote it.
#[test]
fn write_float_matches_go_on_the_s2_corpus() {
    const GO_WRITE_FLOAT: [(u64, &str); 34] = [
        (0x0000_0000_0000_0000, "0"),                       // 0
        (0x8000_0000_0000_0000, "0"),                       // -0
        (0x3ff0_0000_0000_0000, "1"),                       // 1
        (0xbff0_0000_0000_0000, "-1"),                      // -1
        (0x7ff8_0000_0000_0001, "NaN"),                     // NaN
        (0x7ff0_0000_0000_0000, "+Inf"),                    // +Inf
        (0xfff0_0000_0000_0000, "-Inf"),                    // -Inf
        (0x444b_1ae4_d6e2_ef50, "1e+21"),                   // 1e21
        (0x4415_af1d_78b5_8c40, "1e+20"),                   // 1e20
        (0x40fe_2400_0000_0000, "123456"),                  // 123456
        (0x4132_d687_0000_0000, "1.234567e+06"),            // 1234567
        (0x3f1a_36e2_eb1c_432d, "0.0001"),                  // 1e-4
        (0x3ee4_f8b5_88e3_68f1, "1e-05"),                   // 1e-5
        (0x3fd3_3333_3333_3333, "0.3"),                     // 0.1+0.2
        (0x41d9_c68d_b880_0000, "1.729771234e+09"),         // 1.729771234e9
        (0x41d9_c68d_b887_e6b7, "1.7297712341234567e+09"),  // 1729771234.123456789
        (0x41d9_c68d_b887_e6b7, "1.7297712341234567e+09"),  // unixnano/1e9
        (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"), // MaxFloat64
        (0x0000_0000_0000_0001, "5e-324"),                  // SmallestNonzero
        (0x4004_0000_0000_0000, "2.5"),                     // 2.5
        (0x3f74_7ae1_47ae_147b, "0.005"),                   // 0.005
        (0x412e_8480_0000_0000, "1e+06"),                   // 1e6
        (0x40f8_6a00_0000_0000, "100000"),                  // 100000
        (0x412e_847e_0000_0000, "999999"),                  // 999999
        (0x3f20_2c9d_edbc_309d, "0.0001234"),               // 0.0001234
        (0xbe90_c6f7_a0b5_ed8d, "-2.5e-07"),                // -2.5e-7
        (0x4340_0000_0000_0000, "9.007199254740992e+15"),   // 2^53
        (0x0000_0000_0000_0001, "5e-324"),                  // 5e-324
        (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"), // 1.7976931348623157e308
        (0x3fd0_0000_0000_0000, "0.25"),                    // 0.25
        (0x4024_0000_0000_0000, "10"),                      // 10
        (0x4008_0000_0000_0000, "3"),                       // 3
        (0x7ff8_0000_0000_0001, "NaN"),                     // NaN-payload
        (0xfff8_0000_0000_0000, "NaN"),                     // -NaN
    ];
    for (bits, want) in GO_WRITE_FLOAT {
        let mut got = String::new();
        push_float(&mut got, f64::from_bits(bits));
        assert_eq!(got, want, "{bits:016x}");
    }
}

/// The Prometheus Go client's `With(prometheus.Labels)` and `Delete(labels)`: by label name,
/// refusing a map that does not name exactly the family's labels.
#[test]
fn gauge_vec_by_label_map() {
    let vec = GaugeVec::new(Desc::new("by_name", "help", Kind::Gauge, &["b", "a"]).unwrap());
    let labels = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|&(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    };
    vec.set_with(&labels(&[("a", "1"), ("b", "2")]), 7.0)
        .unwrap();
    assert_eq!(
        vec.get(&["2", "1"]),
        Some(7.0),
        "values in label-name order"
    );
    assert_eq!(vec.get_with(&labels(&[("b", "2"), ("a", "1")])), Some(7.0));
    let missing = vec
        .set_with(&labels(&[("a", "1"), ("c", "2")]), 1.0)
        .unwrap_err();
    assert_eq!(
        missing.to_string(),
        "label name \"b\" missing in label map for by_name"
    );
    let extra = vec
        .set_with(&labels(&[("a", "1"), ("b", "2"), ("c", "3")]), 1.0)
        .unwrap_err();
    assert_eq!(
        extra.to_string(),
        "inconsistent label cardinality for by_name: expected 2 label values but got 3"
    );
    assert!(!vec.delete_with(&labels(&[("a", "1")])));
    assert!(vec.delete_with(&labels(&[("a", "1"), ("b", "2")])));
    assert_eq!(vec.get_with(&labels(&[("a", "1"), ("b", "2")])), None);
}

/// The described names include a vec with no series, which a gather leaves out.
#[test]
fn described_names_include_families_without_series() {
    let registry = Registry::new();
    let empty = CounterVec::new(Desc::new("empty_total", "Empty.", Kind::Counter, &["a"]).unwrap());
    registry.register(Arc::new(empty)).unwrap();
    assert!(registry.gather().is_empty());
    assert_eq!(
        registry.described_names().into_iter().collect::<Vec<_>>(),
        ["empty_total"]
    );
}
