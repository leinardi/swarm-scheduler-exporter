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

//! The metric registry, a port of the parts of client_golang the exporter uses.
//!
//! A [`Registry`] holds [`Collector`]s, each describing one or more families by [`Desc`].
//! [`Registry::gather`] collects them and normalises the result as client_golang's `Gather` does:
//! label pairs sorted by name, series by label values, families by name, and families with no
//! series dropped. [`text`] encodes that as expfmt's text format 0.0.4, and [`exposition`] picks
//! the `Content-Type` and `Content-Encoding` the way promhttp negotiates them.
//!
//! The families: [`Gauge`], [`GaugeVec`], [`Counter`], [`CounterVec`], [`Histogram`],
//! [`GaugeFunc`] (evaluated at scrape) and the snapshot families ([`SnapshotCollector`],
//! [`SnapshotFamily`]), which are rebuilt whole and swapped in at once. [`process`] and
//! [`promhttp`] add client_golang's runtime families (Q1).

pub mod exposition;
mod families;
pub mod process;
pub mod promhttp;
mod snapshot;
pub mod text;

#[cfg(test)]
pub(crate) mod testing;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

pub use families::{
    Counter, CounterVec, DEF_BUCKETS, Gauge, GaugeFunc, GaugeVec, Histogram, LabelsMismatch,
};
pub use snapshot::{BuildError, SnapshotBuilder, SnapshotCollector, SnapshotFamily};

/// A family's type, as the text format names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `counter`.
    Counter,
    /// `gauge`.
    Gauge,
    /// `histogram`.
    Histogram,
}

/// A family's descriptor: its name, help text, type and label names (in the order label values
/// are given).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desc {
    name: String,
    help: String,
    kind: Kind,
    label_names: Vec<String>,
}

/// Why a [`Desc`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescError(String);

impl fmt::Display for DescError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DescError {}

impl Desc {
    /// A descriptor, checked as client_golang's `NewDesc` and `NewHistogram` check theirs: a
    /// legacy-valid metric name, legacy-valid and distinct label names not starting with `__`,
    /// and no `le` label on a histogram.
    ///
    /// # Errors
    ///
    /// [`DescError`] naming the first rule broken.
    pub fn new(
        name: impl Into<String>,
        help: impl Into<String>,
        kind: Kind,
        label_names: &[&str],
    ) -> Result<Arc<Self>, DescError> {
        let name = name.into();
        if !text::is_valid_metric_name(&name) {
            return Err(DescError(format!("{name:?} is not a valid metric name")));
        }
        for (index, label) in label_names.iter().enumerate() {
            if !is_valid_label_name(label) || label.starts_with("__") {
                return Err(DescError(format!(
                    "{label:?} is not a valid label name for metric {name:?}"
                )));
            }
            if label_names[..index].contains(label) {
                return Err(DescError(format!(
                    "duplicate label names in constant and variable labels for metric {name:?}"
                )));
            }
            if kind == Kind::Histogram && *label == "le" {
                return Err(DescError(format!(
                    "\"le\" is not allowed as label name in histograms (metric {name:?})"
                )));
            }
        }
        Ok(Arc::new(Self {
            name,
            help: help.into(),
            kind,
            label_names: label_names.iter().map(|&label| label.to_owned()).collect(),
        }))
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

    /// The family type.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The label names, in label-value order.
    #[must_use]
    pub fn label_names(&self) -> &[String] {
        &self.label_names
    }
}

/// The descriptor of a family whose name, help and labels are constants of the exporter: a
/// refusal is a typo the tests catch at once, so it panics rather than return an error.
pub(crate) fn constant_desc(name: &str, help: &str, kind: Kind, label_names: &[&str]) -> Arc<Desc> {
    Desc::new(name, help, kind, label_names)
        .unwrap_or_else(|error| panic!("constant descriptor refused: {error}"))
}

/// A legacy-valid label name: `[a-zA-Z_][a-zA-Z0-9_]*`.
fn is_valid_label_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// A series' value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A counter or gauge value.
    Scalar(f64),
    /// A histogram: the cumulative count at each upper bound (`+Inf` excluded), the sum and the
    /// count.
    Histogram {
        /// `(upper bound, cumulative count)`, in bound order.
        buckets: Vec<(f64, u64)>,
        /// The sum of the observations.
        sum: f64,
        /// The number of observations.
        count: u64,
    },
}

/// One series of a collected family: its label values, in the descriptor's label-name order.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// The label values.
    pub label_values: Vec<String>,
    /// The value.
    pub value: Value,
}

/// A family as a collector returns it.
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    /// Its descriptor.
    pub desc: Arc<Desc>,
    /// Its series.
    pub series: Vec<Series>,
}

/// Something that produces families at scrape time, client_golang's `Collector`.
pub trait Collector: Send + Sync {
    /// The descriptors of every family it may produce.
    fn descs(&self) -> Vec<Arc<Desc>>;

    /// Appends its families, as they are now.
    fn collect(&self, families: &mut Vec<Family>);
}

/// Why a collector was not registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterError {
    name: String,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "duplicate metrics collector registration attempted: {:?} is already registered",
            self.name
        )
    }
}

impl std::error::Error for RegisterError {}

/// A gathered series: its label pairs, sorted by name.
#[derive(Debug, Clone, PartialEq)]
pub struct GatheredSeries {
    /// `(name, value)`, sorted by name.
    pub labels: Vec<(String, String)>,
    /// The value.
    pub value: Value,
}

/// A gathered family, normalised.
#[derive(Debug, Clone, PartialEq)]
pub struct Gathered {
    /// The family name.
    pub name: String,
    /// The help text.
    pub help: String,
    /// The family type.
    pub kind: Kind,
    /// The series, sorted by their label values.
    pub series: Vec<GatheredSeries>,
}

/// A set of collectors, gathered together.
#[derive(Default)]
pub struct Registry {
    collectors: Mutex<Vec<Arc<dyn Collector>>>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Registry")
            .field("collectors", &self.lock().len())
            .finish()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Arc<dyn Collector>>> {
        self.collectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a collector.
    ///
    /// # Errors
    ///
    /// [`RegisterError`] when one of its family names is already registered (client_golang's
    /// `MustRegister` panics there instead; the exporter registers each family once).
    pub fn register(&self, collector: Arc<dyn Collector>) -> Result<(), RegisterError> {
        let mut collectors = self.lock();
        let names: Vec<String> = collector
            .descs()
            .iter()
            .map(|desc| desc.name().to_owned())
            .collect();
        let taken = collectors
            .iter()
            .flat_map(|registered| registered.descs())
            .find(|desc| names.iter().any(|name| name == desc.name()));
        if let Some(desc) = taken {
            return Err(RegisterError {
                name: desc.name().to_owned(),
            });
        }
        collectors.push(collector);
        Ok(())
    }

    /// The name of every family a registered collector describes, whether it has series or not:
    /// what Go's tests read from `Describe`, since a vec with no children never appears in a
    /// gather.
    #[must_use]
    pub fn described_names(&self) -> BTreeSet<String> {
        let collectors: Vec<Arc<dyn Collector>> = self.lock().clone();
        collectors
            .iter()
            .flat_map(|collector| collector.descs())
            .map(|desc| desc.name().to_owned())
            .collect()
    }

    /// Collects every family and normalises them: label pairs by name, series by label values
    /// (a shorter label list first, then the values compared bytewise in label-name order),
    /// families by name, and families with no series dropped.
    #[must_use]
    pub fn gather(&self) -> Vec<Gathered> {
        // Collect outside the lock: a collector may take its own.
        let collectors: Vec<Arc<dyn Collector>> = self.lock().clone();
        let mut families = Vec::new();
        for collector in &collectors {
            collector.collect(&mut families);
        }
        let mut gathered: Vec<Gathered> = families
            .into_iter()
            .filter(|family| !family.series.is_empty())
            .map(normalise)
            .collect();
        gathered.sort_by(|left, right| left.name.cmp(&right.name));
        gathered
    }
}

/// One family, its label pairs and series sorted.
fn normalise(family: Family) -> Gathered {
    let desc = family.desc;
    let mut series: Vec<GatheredSeries> = family
        .series
        .into_iter()
        .map(|series| {
            let mut labels: Vec<(String, String)> = desc
                .label_names()
                .iter()
                .cloned()
                .zip(series.label_values)
                .collect();
            labels.sort_by(|left, right| left.0.cmp(&right.0));
            GatheredSeries {
                labels,
                value: series.value,
            }
        })
        .collect();
    series.sort_by(|left, right| {
        left.labels.len().cmp(&right.labels.len()).then_with(|| {
            left.labels
                .iter()
                .map(|(_, value)| value)
                .cmp(right.labels.iter().map(|(_, value)| value))
        })
    });
    Gathered {
        name: desc.name().to_owned(),
        help: desc.help().to_owned(),
        kind: desc.kind(),
        series,
    }
}

#[cfg(test)]
mod tests;
