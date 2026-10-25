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

//! Test helpers over the registry, ports of the Go tests' `gatherSeries` and `seriesID`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::gofmt_compat::quote::quote;

use super::{Collector, Registry, Value};

/// A series as `name{a="x",b="y"}`, label names sorted and values quoted as Go's `%q`.
#[must_use]
pub(crate) fn series_id(name: &str, labels: &[(&str, &str)]) -> String {
    let mut labels = labels.to_vec();
    labels.sort_unstable();
    let pairs: Vec<String> = labels
        .iter()
        .map(|(label, value)| format!("{label}={}", quote(value)))
        .collect();
    format!("{name}{{{}}}", pairs.join(","))
}

/// Registers `collector` in a fresh registry, gathers it, and returns every scalar series by
/// [`series_id`]. A histogram series fails the test: the Go helper reads gauge values only.
pub(crate) fn gather_series(collector: Arc<dyn Collector>) -> BTreeMap<String, f64> {
    let registry = Registry::new();
    registry
        .register(collector)
        .unwrap_or_else(|error| panic!("register collector: {error}"));
    let mut series = BTreeMap::new();
    for family in registry.gather() {
        for gathered in family.series {
            let labels: Vec<(&str, &str)> = gathered
                .labels
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let Value::Scalar(value) = gathered.value else {
                panic!("{}: not a scalar series", family.name);
            };
            series.insert(series_id(&family.name, &labels), value);
        }
    }
    series
}
