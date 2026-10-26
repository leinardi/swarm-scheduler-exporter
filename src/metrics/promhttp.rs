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

//! promhttp's `InstrumentMetricHandler` metrics (the Prometheus Go client v1.24.1, Q1):
//! `promhttp_metric_handler_requests_total` by status code, with 200, 500 and 503 created at 0,
//! and `promhttp_metric_handler_requests_in_flight`.
//!
//! Go wraps the handler as `InstrumentHandlerCounter(InstrumentHandlerInFlight(handler))`: the
//! in-flight gauge goes up before the handler and down after it, and only then is the request
//! counted, under its status code. So a scrape sees itself in flight and not yet counted, and a
//! handler that panics is never counted. [`HandlerMetrics::begin`] and [`InFlight::finish`] keep
//! that order.

use std::sync::Arc;

use super::{Collector, CounterVec, Gauge, Kind, RegisterError, Registry, constant_desc};

/// The scrape handler's metrics.
#[derive(Debug, Clone)]
pub struct HandlerMetrics {
    requests: Arc<CounterVec>,
    in_flight: Arc<Gauge>,
}

impl Default for HandlerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl HandlerMetrics {
    /// The two families, with the 200, 500 and 503 counters created at 0, unregistered.
    #[must_use]
    pub fn new() -> Self {
        let requests = CounterVec::new(constant_desc(
            "promhttp_metric_handler_requests_total",
            "Total number of scrapes by HTTP status code.",
            Kind::Counter,
            &["code"],
        ));
        for code in ["200", "500", "503"] {
            // One label value for one label name cannot mismatch.
            let _ = requests.add(&[code], 0.0);
        }
        let in_flight = Gauge::new(constant_desc(
            "promhttp_metric_handler_requests_in_flight",
            "Current number of scrapes being served.",
            Kind::Gauge,
            &[],
        ));
        Self {
            requests: Arc::new(requests),
            in_flight: Arc::new(in_flight),
        }
    }

    /// Registers both families.
    ///
    /// # Errors
    ///
    /// [`RegisterError`] when either name is taken.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        registry.register(Arc::clone(&self.requests) as Arc<dyn Collector>)?;
        registry.register(Arc::clone(&self.in_flight) as Arc<dyn Collector>)
    }

    /// Marks a scrape in flight until the returned guard is finished or dropped.
    pub fn begin(&self) -> InFlight {
        self.in_flight.add(1.0);
        InFlight {
            metrics: self.clone(),
        }
    }
}

/// A scrape in flight.
#[derive(Debug)]
#[must_use = "dropping it ends the scrape without counting it"]
pub struct InFlight {
    metrics: HandlerMetrics,
}

impl InFlight {
    /// Ends the scrape and counts it under `status`, as promhttp's `sanitizeCode` names it.
    pub fn finish(self, status: u16) {
        let requests = Arc::clone(&self.metrics.requests);
        // The in-flight gauge goes down first, as in Go.
        drop(self);
        // One label value for one label name cannot mismatch.
        let _ = requests.inc(&[&sanitize_code(status)]);
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.metrics.in_flight.add(-1.0);
    }
}

/// promhttp's `sanitizeCode`: 0 (no status written) is 200, 100 to 599 are their number, anything
/// else is `unknown`.
#[must_use]
pub fn sanitize_code(status: u16) -> String {
    match status {
        0 => "200".to_owned(),
        100..=599 => status.to_string(),
        _ => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests;
