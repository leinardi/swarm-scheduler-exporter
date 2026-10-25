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

//! The plain families: one value per series, kept between scrapes.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{Collector, Desc, Family, Series, Value};

/// client_golang's `DefBuckets`: 5ms to 10s.
pub const DEF_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Label values whose count is not the family's number of label names (client_golang's
/// `WithLabelValues` panics there instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelsMismatch {
    family: String,
    want: usize,
    got: usize,
    // A label name the label map lacks (client_golang's `With(prometheus.Labels)`).
    missing: Option<String>,
}

impl fmt::Display for LabelsMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(missing) = &self.missing {
            return write!(
                formatter,
                "label name {missing:?} missing in label map for {}",
                self.family
            );
        }
        write!(
            formatter,
            "inconsistent label cardinality for {}: expected {} label values but got {}",
            self.family, self.want, self.got
        )
    }
}

impl std::error::Error for LabelsMismatch {}

/// An `f64` updated atomically, as client_golang keeps gauge and counter values.
#[derive(Debug, Default)]
struct AtomicF64(AtomicU64);

impl AtomicF64 {
    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(AtomicOrdering::Relaxed))
    }

    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), AtomicOrdering::Relaxed);
    }

    fn add(&self, delta: f64) {
        let _ = self
            .0
            .fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |bits| {
                Some((f64::from_bits(bits) + delta).to_bits())
            });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The label values as owned strings, checked against the family.
fn owned_values(desc: &Desc, label_values: &[&str]) -> Result<Vec<String>, LabelsMismatch> {
    if label_values.len() != desc.label_names().len() {
        return Err(LabelsMismatch {
            family: desc.name().to_owned(),
            want: desc.label_names().len(),
            got: label_values.len(),
            missing: None,
        });
    }
    Ok(label_values.iter().map(|&value| value.to_owned()).collect())
}

/// client_golang's `With(prometheus.Labels)`: the values of `labels` in label-name order, which
/// must name exactly the family's labels.
fn values_in_order<'labels>(
    desc: &Desc,
    labels: &'labels BTreeMap<String, String>,
) -> Result<Vec<&'labels str>, LabelsMismatch> {
    let mismatch = |missing| LabelsMismatch {
        family: desc.name().to_owned(),
        want: desc.label_names().len(),
        got: labels.len(),
        missing,
    };
    if labels.len() != desc.label_names().len() {
        return Err(mismatch(None));
    }
    desc.label_names()
        .iter()
        .map(|name| {
            labels
                .get(name)
                .map(String::as_str)
                .ok_or_else(|| mismatch(Some(name.clone())))
        })
        .collect()
}

/// One scalar series with no labels.
fn scalar_family(desc: &Arc<Desc>, value: f64) -> Family {
    Family {
        desc: Arc::clone(desc),
        series: vec![Series {
            label_values: Vec::new(),
            value: Value::Scalar(value),
        }],
    }
}

/// The series of a vec, as a family.
fn vec_family(desc: &Arc<Desc>, children: &BTreeMap<Vec<String>, f64>) -> Family {
    Family {
        desc: Arc::clone(desc),
        series: children
            .iter()
            .map(|(label_values, &value)| Series {
                label_values: label_values.clone(),
                value: Value::Scalar(value),
            })
            .collect(),
    }
}

/// A gauge without labels.
#[derive(Debug)]
pub struct Gauge {
    desc: Arc<Desc>,
    value: AtomicF64,
}

impl Gauge {
    /// A gauge at 0. `desc` must be a gauge descriptor without labels.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            desc,
            value: AtomicF64::default(),
        }
    }

    /// Sets the value.
    pub fn set(&self, value: f64) {
        self.value.store(value);
    }

    /// Adds `delta`, which may be negative.
    pub fn add(&self, delta: f64) {
        self.value.add(delta);
    }

    /// The value.
    #[must_use]
    pub fn get(&self) -> f64 {
        self.value.load()
    }
}

impl Collector for Gauge {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        families.push(scalar_family(&self.desc, self.get()));
    }
}

/// A gauge with labels, one series per label-value list, each kept until deleted.
#[derive(Debug)]
pub struct GaugeVec {
    desc: Arc<Desc>,
    children: Mutex<BTreeMap<Vec<String>, f64>>,
}

impl GaugeVec {
    /// A vec with no series.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            desc,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// Sets the series of `label_values` (in label-name order), creating it.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the count of values is wrong.
    pub fn set(&self, label_values: &[&str], value: f64) -> Result<(), LabelsMismatch> {
        let key = owned_values(&self.desc, label_values)?;
        lock(&self.children).insert(key, value);
        Ok(())
    }

    /// Deletes the series of `label_values`; whether it existed.
    pub fn delete(&self, label_values: &[&str]) -> bool {
        let key: Vec<String> = label_values.iter().map(|&value| value.to_owned()).collect();
        lock(&self.children).remove(&key).is_some()
    }

    /// The value of the series of `label_values`, if it exists.
    #[must_use]
    pub fn get(&self, label_values: &[&str]) -> Option<f64> {
        let key: Vec<String> = label_values.iter().map(|&value| value.to_owned()).collect();
        lock(&self.children).get(&key).copied()
    }

    /// client_golang's `With(labels).Set(value)`: [`GaugeVec::set`] by label name.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when `labels` does not name exactly the family's labels.
    pub fn set_with(
        &self,
        labels: &BTreeMap<String, String>,
        value: f64,
    ) -> Result<(), LabelsMismatch> {
        self.set(&values_in_order(&self.desc, labels)?, value)
    }

    /// client_golang's `Delete(labels)`: whether the series existed; false as well when `labels`
    /// does not name exactly the family's labels.
    pub fn delete_with(&self, labels: &BTreeMap<String, String>) -> bool {
        values_in_order(&self.desc, labels).is_ok_and(|values| self.delete(&values))
    }

    /// The value of the series `labels` name, if it exists.
    #[must_use]
    pub fn get_with(&self, labels: &BTreeMap<String, String>) -> Option<f64> {
        values_in_order(&self.desc, labels)
            .ok()
            .and_then(|values| self.get(&values))
    }
}

impl Collector for GaugeVec {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        families.push(vec_family(&self.desc, &lock(&self.children)));
    }
}

/// A counter without labels.
#[derive(Debug)]
pub struct Counter {
    desc: Arc<Desc>,
    value: AtomicF64,
}

impl Counter {
    /// A counter at 0.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            desc,
            value: AtomicF64::default(),
        }
    }

    /// Adds 1.
    pub fn inc(&self) {
        self.add(1.0);
    }

    /// Adds `delta`. A counter cannot decrease: a negative `delta` is ignored (client_golang
    /// panics; the exporter never passes one).
    pub fn add(&self, delta: f64) {
        if delta >= 0.0 {
            self.value.add(delta);
        }
    }

    /// The value.
    #[must_use]
    pub fn get(&self) -> f64 {
        self.value.load()
    }
}

impl Collector for Counter {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        families.push(scalar_family(&self.desc, self.get()));
    }
}

/// A counter with labels.
#[derive(Debug)]
pub struct CounterVec {
    desc: Arc<Desc>,
    children: Mutex<BTreeMap<Vec<String>, f64>>,
}

impl CounterVec {
    /// A vec with no series.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            desc,
            children: Mutex::new(BTreeMap::new()),
        }
    }

    /// Adds `delta` to the series of `label_values`, creating it at 0 first; a negative `delta`
    /// adds nothing but still creates the series. `add(values, 0.0)` pre-creates a series, as
    /// `WithLabelValues` does.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the count of values is wrong.
    pub fn add(&self, label_values: &[&str], delta: f64) -> Result<(), LabelsMismatch> {
        let key = owned_values(&self.desc, label_values)?;
        let mut children = lock(&self.children);
        let value = children.entry(key).or_insert(0.0);
        if delta >= 0.0 {
            *value += delta;
        }
        Ok(())
    }

    /// Adds 1 to the series of `label_values`.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the count of values is wrong.
    pub fn inc(&self, label_values: &[&str]) -> Result<(), LabelsMismatch> {
        self.add(label_values, 1.0)
    }

    /// The value of the series of `label_values`, if it exists.
    #[must_use]
    pub fn get(&self, label_values: &[&str]) -> Option<f64> {
        let key: Vec<String> = label_values.iter().map(|&value| value.to_owned()).collect();
        lock(&self.children).get(&key).copied()
    }
}

impl Collector for CounterVec {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        families.push(vec_family(&self.desc, &lock(&self.children)));
    }
}

/// The observations of a histogram, counted per bucket (not cumulatively).
#[derive(Debug)]
struct HistogramState {
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

/// A histogram without labels, with explicit upper bounds.
#[derive(Debug)]
pub struct Histogram {
    desc: Arc<Desc>,
    upper_bounds: Vec<f64>,
    state: Mutex<HistogramState>,
}

impl Histogram {
    /// A histogram with `upper_bounds` (increasing; a final `+Inf` is implied and dropped, as
    /// client_golang does).
    #[must_use]
    pub fn new(desc: Arc<Desc>, upper_bounds: &[f64]) -> Self {
        let mut upper_bounds = upper_bounds.to_vec();
        if upper_bounds
            .last()
            .is_some_and(|bound| *bound == f64::INFINITY)
        {
            upper_bounds.pop();
        }
        let buckets = upper_bounds.len();
        Self {
            desc,
            upper_bounds,
            state: Mutex::new(HistogramState {
                counts: vec![0; buckets],
                sum: 0.0,
                count: 0,
            }),
        }
    }

    /// Records `value` in the first bucket whose bound is at least `value` (Go's
    /// `sort.SearchFloat64s`, so a NaN lands in `+Inf` only), and in the sum and count.
    pub fn observe(&self, value: f64) {
        let bucket = self.upper_bounds.partition_point(|bound| {
            matches!(bound.partial_cmp(&value), Some(Ordering::Less) | None)
        });
        let mut state = lock(&self.state);
        if let Some(count) = state.counts.get_mut(bucket) {
            *count += 1;
        }
        state.sum += value;
        state.count += 1;
    }
}

impl Collector for Histogram {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        let state = lock(&self.state);
        let mut cumulative = 0;
        let buckets = self
            .upper_bounds
            .iter()
            .zip(&state.counts)
            .map(|(&bound, &count)| {
                cumulative += count;
                (bound, cumulative)
            })
            .collect();
        families.push(Family {
            desc: Arc::clone(&self.desc),
            series: vec![Series {
                label_values: Vec::new(),
                value: Value::Histogram {
                    buckets,
                    sum: state.sum,
                    count: state.count,
                },
            }],
        });
    }
}

/// A gauge whose value is read from a function at every scrape.
pub struct GaugeFunc {
    desc: Arc<Desc>,
    function: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl fmt::Debug for GaugeFunc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GaugeFunc")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

impl GaugeFunc {
    /// A gauge reading `function` at scrape time.
    #[must_use]
    pub fn new(desc: Arc<Desc>, function: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self {
            desc,
            function: Box::new(function),
        }
    }
}

impl Collector for GaugeFunc {
    fn descs(&self) -> Vec<Arc<Desc>> {
        vec![Arc::clone(&self.desc)]
    }

    fn collect(&self, families: &mut Vec<Family>) {
        families.push(scalar_family(&self.desc, (self.function)()));
    }
}
