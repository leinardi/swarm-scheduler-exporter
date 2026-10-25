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

//! Gauge families rebuilt whole on every update. A [`GaugeVec`](super::GaugeVec) rebuilt with
//! one `set` per series is locked per call, not across the rebuild, so a scrape in between would
//! see it half written. Here the new set is built off to the side with a [`SnapshotBuilder`] and
//! published with one swap of an `Arc`, which a scrape loads once: it sees the previous set or
//! the new one, never a mix, and families published together are always from the same update.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use super::{Collector, Desc, Family, Series, Value};

/// A label set whose keys are not exactly its family's label names (client_golang's
/// `GaugeVec.With` panics on the same input; a build fails instead).
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

/// Go's `%v` of a `[]string`: `[a b]`.
fn go_list(names: &[String]) -> String {
    format!("[{}]", names.join(" "))
}

/// One or more gauge families, published together.
#[derive(Debug)]
pub struct SnapshotCollector {
    descs: Vec<Arc<Desc>>,
    current: Mutex<Option<Arc<Vec<Family>>>>,
}

impl SnapshotCollector {
    /// A collector for `descs`, which are gauge descriptors, with nothing published: until the
    /// first publish it produces no series, like a vec with no children.
    #[must_use]
    pub fn new(descs: Vec<Arc<Desc>>) -> Self {
        Self {
            descs,
            current: Mutex::new(None),
        }
    }

    /// Replaces the published set in one swap. `families` must be the complete set of every
    /// family of the collector, as [`SnapshotBuilder::build`] returns it.
    pub fn publish(&self, families: Vec<Family>) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(families));
    }

    /// The published set, loaded once: a publish after this call does not change it.
    #[must_use]
    pub fn load(&self) -> Option<Arc<Vec<Family>>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Collector for SnapshotCollector {
    fn descs(&self) -> Vec<Arc<Desc>> {
        self.descs.clone()
    }

    fn collect(&self, families: &mut Vec<Family>) {
        if let Some(published) = self.load() {
            families.extend(published.iter().cloned());
        }
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
    fn descs(&self) -> Vec<Arc<Desc>> {
        self.collector.descs()
    }

    fn collect(&self, families: &mut Vec<Family>) {
        self.collector.collect(families);
    }
}

/// The series of one family in a build: by series key, the label values and value.
type FamilySeries = BTreeMap<String, (Vec<String>, f64)>;

/// The series of one update, before it is published.
#[derive(Debug, Default)]
pub struct SnapshotBuilder {
    families: HashMap<String, (Arc<Desc>, FamilySeries)>,
    error: Option<BuildError>,
}

impl SnapshotBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `value` for the series of `desc` with `labels` (name, value), which must name
    /// exactly the family's labels; a name given twice keeps its last value, as a Go map does.
    /// Setting the same label set twice keeps the last value. A wrong label set fails the
    /// build, with the first error kept.
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
                go_list(label_names)
            )));
            return;
        }
        let mut label_values = Vec::with_capacity(label_names.len());
        for name in label_names {
            let Some(&label_value) = labels.get(name.as_str()) else {
                self.fail(BuildError(format!(
                    "missing {}, want {}",
                    crate::gofmt_compat::quote::quote(name),
                    go_list(label_names)
                )));
                return;
            };
            label_values.push(label_value.to_owned());
        }
        let (_, series) = self
            .families
            .entry(desc.name().to_owned())
            .or_insert_with(|| (Arc::clone(desc), BTreeMap::new()));
        series.insert(series_key(&label_values), (label_values, value));
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
    pub fn build(self) -> Result<Vec<Family>, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(self
            .families
            .into_values()
            .map(|(desc, series)| Family {
                desc,
                series: series
                    .into_values()
                    .map(|(label_values, value)| Series {
                        label_values,
                        value: Value::Scalar(value),
                    })
                    .collect(),
            })
            .collect())
    }
}

/// Encodes label values as a map key. Each value is prefixed with its length, so two different
/// value lists never share a key whatever characters they contain: joining with a separator
/// would make `["a|b", "c"]` and `["a", "b|c"]` collide, and custom label values come from users.
#[must_use]
pub(crate) fn series_key(label_values: &[String]) -> String {
    let mut key = String::new();
    for value in label_values {
        key.push_str(&value.len().to_string());
        key.push(':');
        key.push_str(value);
    }
    key
}
