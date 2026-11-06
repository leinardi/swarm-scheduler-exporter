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

//! The metric building blocks over prometheus-client, for the families whose label names are
//! only known at startup (the custom labels) or whose series are replaced as a whole:
//!
//! - [`GaugeVec`]: a gauge with labels, one series per label set, each kept until deleted.
//! - [`SnapshotCollector`] and [`SnapshotBuilder`]: gauge families built whole from one poll and
//!   swapped in at once, so a scrape sees one poll or the next, never a mix.
//! - [`ScrapeGauge`]: a gauge whose value is computed at scrape time.
//!
//! The fixed families (the counters, the histogram) use prometheus-client's own types. A family
//! with no series encodes nothing, as a vec with no children does in the Prometheus Go client.
//! Series are written in label-value order, labels in name order.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use prometheus_client::collector::Collector;
use prometheus_client::encoding::{DescriptorEncoder, EncodeMetric};
use prometheus_client::metrics::MetricType;
use prometheus_client::registry::Registry;

/// The default histogram buckets, as the Prometheus Go client's `DefBuckets`.
pub const DEF_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// A family's name, help text and label names.
#[derive(Debug, PartialEq, Eq)]
pub struct Desc {
    name: String,
    help: String,
    label_names: Vec<String>,
}

impl Desc {
    /// A descriptor of the gauge `name`, with `label_names` in the order a series lists its
    /// values.
    #[must_use]
    pub fn new(name: &str, help: &str, label_names: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            help: help.to_owned(),
            label_names: label_names.iter().map(|&label| label.to_owned()).collect(),
        })
    }

    /// The family name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The help text.
    #[must_use]
    pub fn help(&self) -> &str {
        &self.help
    }

    /// The label names.
    #[must_use]
    pub fn label_names(&self) -> &[String] {
        &self.label_names
    }

    /// `values` paired with the label names, sorted by name.
    fn pairs(&self, values: &[String]) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .label_names
            .iter()
            .cloned()
            .zip(values.iter().cloned())
            .collect();
        pairs.sort();
        pairs
    }
}

/// A label set whose names are not exactly the family's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelsMismatch {
    family: String,
    want: usize,
    got: usize,
    // A label name the label map lacks.
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

/// The label values of `labels` in the label-name order of `desc`.
fn values_in_order(
    desc: &Desc,
    labels: &BTreeMap<String, String>,
) -> Result<Vec<String>, LabelsMismatch> {
    let mismatch = |missing: Option<String>| LabelsMismatch {
        family: desc.name.clone(),
        want: desc.label_names.len(),
        got: labels.len(),
        missing,
    };
    if labels.len() != desc.label_names.len() {
        return Err(mismatch(None));
    }
    desc.label_names
        .iter()
        .map(|name| {
            labels
                .get(name)
                .cloned()
                .ok_or_else(|| mismatch(Some(name.clone())))
        })
        .collect()
}

/// Panics on a label set that does not fit its family: the exporter builds its label sets from
/// the family's own names, so a mismatch is a programming error, which the reconciler's step
/// recovery catches.
pub(crate) fn must(result: Result<(), LabelsMismatch>) {
    if let Err(error) = result {
        panic!("{error}");
    }
}

/// Registers `collector` on `registry`.
pub fn register<C: Collector>(registry: &mut Registry, collector: &Arc<C>) {
    registry.register_collector(Box::new(Arc::clone(collector)));
}

/// Encodes one gauge family: its descriptor, then a series per `(label values, value)`. Nothing
/// when there are no series.
fn encode_gauge_family<'a>(
    encoder: &mut DescriptorEncoder<'_>,
    desc: &Desc,
    series: impl IntoIterator<Item = (&'a Vec<String>, &'a f64)>,
) -> Result<(), fmt::Error> {
    let mut series = series.into_iter().peekable();
    if series.peek().is_none() {
        return Ok(());
    }
    let mut metric = encoder.encode_descriptor(&desc.name, &desc.help, None, MetricType::Gauge)?;
    for (values, value) in series {
        metric
            .encode_family(&desc.pairs(values))?
            .encode_gauge(value)?;
    }
    Ok(())
}

/// A gauge with labels: one series per label-value list, each kept until deleted.
#[derive(Debug)]
pub struct GaugeVec {
    desc: Arc<Desc>,
    children: RwLock<BTreeMap<Vec<String>, f64>>,
}

impl GaugeVec {
    /// A vec with no series.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            desc,
            children: RwLock::new(BTreeMap::new()),
        }
    }

    /// The descriptor.
    #[must_use]
    pub fn desc(&self) -> &Arc<Desc> {
        &self.desc
    }

    /// Sets the series of `label_values` (in label-name order), creating it.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the count of values is wrong.
    pub fn set(&self, label_values: &[&str], value: f64) -> Result<(), LabelsMismatch> {
        if label_values.len() != self.desc.label_names.len() {
            return Err(LabelsMismatch {
                family: self.desc.name.clone(),
                want: self.desc.label_names.len(),
                got: label_values.len(),
                missing: None,
            });
        }
        let key = label_values.iter().map(|&value| value.to_owned()).collect();
        self.children
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
        Ok(())
    }

    /// Sets the series `labels` name.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when `labels` does not name exactly the family's labels.
    pub fn set_with(
        &self,
        labels: &BTreeMap<String, String>,
        value: f64,
    ) -> Result<(), LabelsMismatch> {
        let key = values_in_order(&self.desc, labels)?;
        self.children
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
        Ok(())
    }

    /// Deletes the series `labels` name: whether it existed; false as well when `labels` does
    /// not name exactly the family's labels.
    pub fn delete_with(&self, labels: &BTreeMap<String, String>) -> bool {
        values_in_order(&self.desc, labels).is_ok_and(|key| {
            self.children
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key)
                .is_some()
        })
    }

    /// The value of the series `labels` name, if it exists.
    #[must_use]
    pub fn get_with(&self, labels: &BTreeMap<String, String>) -> Option<f64> {
        let key = values_in_order(&self.desc, labels).ok()?;
        self.children
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied()
    }
}

impl Collector for GaugeVec {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        let children = self
            .children
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        encode_gauge_family(&mut encoder, &self.desc, &children)
    }
}

/// A gauge without labels whose value `read` computes at every scrape.
pub struct ScrapeGauge {
    name: String,
    help: String,
    read: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl ScrapeGauge {
    /// The gauge `name`, reading `read` on every scrape.
    #[must_use]
    pub fn new(
        name: &str,
        help: &str,
        read: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            help: help.to_owned(),
            read: Box::new(read),
        })
    }

    /// The value a scrape would report now.
    #[must_use]
    pub fn value(&self) -> f64 {
        (self.read)()
    }
}

impl fmt::Debug for ScrapeGauge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScrapeGauge")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Collector for ScrapeGauge {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        let metric = encoder.encode_descriptor(&self.name, &self.help, None, MetricType::Gauge)?;
        prometheus_client::metrics::gauge::ConstGauge::new(self.value()).encode(metric)
    }
}

/// A label set whose keys are not exactly its family's label names: a build fails instead of
/// publishing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError(String);

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "label set does not match the family's label names: {}",
            self.0
        )
    }
}

impl std::error::Error for BuildError {}

/// `[a b]`, a list of names.
fn list(names: &[String]) -> String {
    format!("[{}]", names.join(" "))
}

/// One built family: its descriptor and series, by label values.
#[derive(Debug, Clone)]
pub struct BuiltFamily {
    desc: Arc<Desc>,
    series: BTreeMap<Vec<String>, f64>,
}

impl BuiltFamily {
    /// The descriptor.
    #[must_use]
    pub fn desc(&self) -> &Arc<Desc> {
        &self.desc
    }

    /// The series: label values in label-name order, and the value.
    pub fn series(&self) -> impl Iterator<Item = (&Vec<String>, f64)> {
        self.series.iter().map(|(values, value)| (values, *value))
    }
}

/// One or more gauge families, published together: the whole set is replaced in one swap, and a
/// scrape loads it once.
#[derive(Debug)]
pub struct SnapshotCollector {
    descs: Vec<Arc<Desc>>,
    current: RwLock<Option<Arc<Vec<BuiltFamily>>>>,
}

impl SnapshotCollector {
    /// A collector for the families `descs`, with nothing published: until the first publish it
    /// encodes no series.
    #[must_use]
    pub fn new(descs: Vec<Arc<Desc>>) -> Self {
        Self {
            descs,
            current: RwLock::new(None),
        }
    }

    /// The families' descriptors.
    #[must_use]
    pub fn descs(&self) -> Vec<Arc<Desc>> {
        self.descs.clone()
    }

    /// Replaces the published set in one swap. `families` must be the complete set of every
    /// family of the collector, as [`SnapshotBuilder::build`] returns it.
    pub fn publish(&self, families: Vec<BuiltFamily>) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(families));
    }

    /// The published set, loaded once: a publish after this call does not change it.
    #[must_use]
    pub fn load(&self) -> Option<Arc<Vec<BuiltFamily>>> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Collector for SnapshotCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        let Some(published) = self.load() else {
            return Ok(());
        };
        let mut families: Vec<&BuiltFamily> = published.iter().collect();
        families.sort_by(|left, right| left.desc.name.cmp(&right.desc.name));
        for family in families {
            encode_gauge_family(&mut encoder, &family.desc, &family.series)?;
        }
        Ok(())
    }
}

/// A snapshot collector of a single gauge family.
#[derive(Debug)]
pub struct SnapshotFamily {
    collector: SnapshotCollector,
    desc: Arc<Desc>,
}

impl SnapshotFamily {
    /// A single-family snapshot collector with nothing published.
    #[must_use]
    pub fn new(desc: Arc<Desc>) -> Self {
        Self {
            collector: SnapshotCollector::new(vec![Arc::clone(&desc)]),
            desc,
        }
    }

    /// The family's descriptor.
    #[must_use]
    pub fn desc(&self) -> &Arc<Desc> {
        &self.desc
    }

    /// The collector, to publish to.
    #[must_use]
    pub fn collector(&self) -> &SnapshotCollector {
        &self.collector
    }
}

impl Collector for SnapshotFamily {
    fn encode(&self, encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        self.collector.encode(encoder)
    }
}

/// The series of one update, before it is published.
#[derive(Debug, Default)]
pub struct SnapshotBuilder {
    families: HashMap<String, BuiltFamily>,
    error: Option<BuildError>,
}

impl SnapshotBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `value` for the series of `desc` with `labels` (name, value), which must name
    /// exactly the family's labels; a name given twice keeps its last value. Setting the same
    /// label set twice keeps the last value. A wrong label set fails the build, with the first
    /// error kept.
    pub fn set<K: AsRef<str>, V: AsRef<str>>(
        &mut self,
        desc: &Arc<Desc>,
        labels: &[(K, V)],
        value: f64,
    ) {
        let labels: HashMap<&str, &str> = labels
            .iter()
            .map(|(name, value)| (name.as_ref(), value.as_ref()))
            .collect();
        let label_names = desc.label_names();
        if labels.len() != label_names.len() {
            self.fail(BuildError(format!(
                "got {} labels, want {}",
                labels.len(),
                list(label_names)
            )));
            return;
        }
        let mut label_values = Vec::with_capacity(label_names.len());
        for name in label_names {
            let Some(&label_value) = labels.get(name.as_str()) else {
                self.fail(BuildError(format!(
                    "missing {name:?}, want {}",
                    list(label_names)
                )));
                return;
            };
            label_values.push(label_value.to_owned());
        }
        self.families
            .entry(desc.name().to_owned())
            .or_insert_with(|| BuiltFamily {
                desc: Arc::clone(desc),
                series: BTreeMap::new(),
            })
            .series
            .insert(label_values, value);
    }

    /// Keeps the first error: later ones add nothing a fix would not also fix.
    fn fail(&mut self, error: BuildError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    /// The recorded families, one per family set at least once. Any error fails the whole build
    /// and returns nothing, so the caller keeps the previous set published rather than a partial
    /// one.
    ///
    /// # Errors
    ///
    /// The first [`BuildError`] recorded.
    pub fn build(self) -> Result<Vec<BuiltFamily>, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(self.families.into_values().collect())
    }
}

/// Encodes `registry` as OpenMetrics text.
///
/// # Panics
///
/// Never in practice: encoding into a `String` does not fail.
#[must_use]
pub fn render(registry: &Registry) -> String {
    let mut body = String::new();
    prometheus_client::encoding::text::encode(&mut body, registry)
        .unwrap_or_else(|error| panic!("encode the exposition: {error}"));
    body
}

/// The `Content-Type` of [`render`]'s body.
pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

#[cfg(test)]
mod tests;
