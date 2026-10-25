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

//! The service update and rollback metrics: an info-style state family, one series per known
//! state with 1 on the current one, and the started and completed timestamps.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::docker::types::{GoTime, Service};
use crate::gofmt_compat::float::from_int;
use crate::logger::Logger;
use crate::metrics::{
    Collector, GaugeVec, Kind, LabelsMismatch, RegisterError, Registry, constant_desc,
};

use super::desired::labels_for_metadata;
use super::metrics_ids::{
    LABEL_DISPLAY_NAME, LABEL_SERVICE, LABEL_SERVICE_MODE, LABEL_STACK, LABEL_STATE, NAMESPACE,
    SERVICE_SUBSYSTEM,
};
use super::types::{CustomLabels, ServiceMetadata};

/// The exported update states.
pub const UPDATE_STATE_UPDATING: &str = "updating";
/// `completed`, also the state of a service that never updated.
pub const UPDATE_STATE_COMPLETED: &str = "completed";
/// `paused`, also the state of `rollback_paused` and of any value the exporter does not know.
pub const UPDATE_STATE_PAUSED: &str = "paused";
/// `rollback_started`.
pub const UPDATE_STATE_ROLLBACK_STARTED: &str = "rollback_started";
/// `rollback_completed`.
pub const UPDATE_STATE_ROLLBACK_COMPLETED: &str = "rollback_completed";

/// Every exported state, in the order the series are written.
pub const KNOWN_STATES: [&str; 5] = [
    UPDATE_STATE_UPDATING,
    UPDATE_STATE_COMPLETED,
    UPDATE_STATE_PAUSED,
    UPDATE_STATE_ROLLBACK_STARTED,
    UPDATE_STATE_ROLLBACK_COMPLETED,
];

/// The HELP text of `swarm_service_update_state_info`.
pub const UPDATE_STATE_HELP: &str =
    "Service update/rollback state indicator (1 for the current state, 0 otherwise).";
/// The HELP text of `swarm_service_update_started_timestamp_seconds`.
pub const UPDATE_STARTED_HELP: &str =
    "Unix timestamp (seconds) when the current/last service update started (0 if unknown).";
/// The HELP text of `swarm_service_update_completed_timestamp_seconds`.
pub const UPDATE_COMPLETED_HELP: &str =
    "Unix timestamp (seconds) when the current/last service update completed (0 if unknown).";

/// Maps the service's `UpdateStatus.State` (`None` when it has no update status) to the
/// exported state.
#[must_use]
#[expect(
    clippy::match_same_arms,
    reason = "the fallback arm stays apart from the paused states it shares a value with, so a \
              new known state is a new arm and only an unknown value takes the fallback"
)]
pub fn update_state_from_swarm(state: Option<&str>) -> &'static str {
    match state {
        None | Some("completed") => UPDATE_STATE_COMPLETED,
        Some("updating") => UPDATE_STATE_UPDATING,
        Some("paused" | "rollback_paused") => UPDATE_STATE_PAUSED,
        Some("rollback_started") => UPDATE_STATE_ROLLBACK_STARTED,
        Some("rollback_completed") => UPDATE_STATE_ROLLBACK_COMPLETED,
        // An unknown value, the empty string included, is paused: neutral, not advancing.
        Some(_) => UPDATE_STATE_PAUSED,
    }
}

/// A copy of `base` with the `state` label set.
#[must_use]
pub fn clone_labels_with_state(
    base: &BTreeMap<String, String>,
    state: &str,
) -> BTreeMap<String, String> {
    let mut labels = base.clone();
    labels.insert(LABEL_STATE.to_owned(), state.to_owned());
    labels
}

/// The three families.
#[derive(Debug)]
struct Families {
    state: Arc<GaugeVec>,
    started: Arc<GaugeVec>,
    completed: Arc<GaugeVec>,
}

/// The service update metrics; before they are configured, their use is an error, logged once.
#[derive(Debug)]
pub struct ServiceUpdateMetrics {
    families: Option<Families>,
    logger: Logger,
    warned: AtomicBool,
}

impl ServiceUpdateMetrics {
    /// The families, labelled with the base labels plus `custom_labels`, and `state` last on
    /// the state family.
    #[must_use]
    pub fn new(custom_labels: &CustomLabels, logger: Logger) -> Self {
        let mut base = vec![
            LABEL_STACK.to_owned(),
            LABEL_SERVICE.to_owned(),
            LABEL_SERVICE_MODE.to_owned(),
            LABEL_DISPLAY_NAME.to_owned(),
        ];
        base.extend(custom_labels.sanitized_names());
        let mut with_state = base.clone();
        with_state.push(LABEL_STATE.to_owned());
        let family = |name: &str, help: &str, labels: &[String]| {
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            Arc::new(GaugeVec::new(constant_desc(
                &format!("{NAMESPACE}_{SERVICE_SUBSYSTEM}_{name}"),
                help,
                Kind::Gauge,
                &labels,
            )))
        };
        Self {
            families: Some(Families {
                state: family("update_state_info", UPDATE_STATE_HELP, &with_state),
                started: family(
                    "update_started_timestamp_seconds",
                    UPDATE_STARTED_HELP,
                    &base,
                ),
                completed: family(
                    "update_completed_timestamp_seconds",
                    UPDATE_COMPLETED_HELP,
                    &base,
                ),
            }),
            logger,
            warned: AtomicBool::new(false),
        }
    }

    /// Metrics that were never configured: the Go exporter's nil families.
    #[must_use]
    pub fn unconfigured(logger: Logger) -> Self {
        Self {
            families: None,
            logger,
            warned: AtomicBool::new(false),
        }
    }

    /// Registers the three families; nothing when unconfigured.
    ///
    /// # Errors
    ///
    /// The registry's refusal.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        let Some(families) = &self.families else {
            return Ok(());
        };
        for family in [&families.state, &families.started, &families.completed] {
            registry.register(Arc::clone(family) as Arc<dyn Collector>)?;
        }
        Ok(())
    }

    /// The families when configured, else the error, logged the first time only.
    fn families(&self, action: &str) -> Option<&Families> {
        if self.families.is_none() && !self.warned.swap(true, Ordering::SeqCst) {
            self.logger.error(
                &format!("service update metrics not configured before use; skipping {action}"),
                &[],
            );
        }
        self.families.as_ref()
    }

    /// The state family.
    #[must_use]
    pub fn state(&self) -> Option<&GaugeVec> {
        self.families.as_ref().map(|families| &*families.state)
    }

    /// The started-timestamp family.
    #[must_use]
    pub fn started(&self) -> Option<&GaugeVec> {
        self.families.as_ref().map(|families| &*families.started)
    }

    /// The completed-timestamp family.
    #[must_use]
    pub fn completed(&self) -> Option<&GaugeVec> {
        self.families.as_ref().map(|families| &*families.completed)
    }

    /// Writes every known state of `service` (1 for the current one, completed without an
    /// update status) and its timestamps, in whole seconds, 0 when unknown.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the metadata's custom labels are not the families'.
    pub fn update_for_service(
        &self,
        service: &Service,
        metadata: &ServiceMetadata,
    ) -> Result<(), LabelsMismatch> {
        let Some(families) = self.families("emission") else {
            return Ok(());
        };
        let base = labels_for_metadata(metadata);
        let status = service.update_status.as_ref();
        let current = update_state_from_swarm(status.map(|status| status.state.as_str()));
        for state in KNOWN_STATES {
            let value = if state == current { 1.0 } else { 0.0 };
            families
                .state
                .set_with(&clone_labels_with_state(&base, state), value)?;
        }
        let seconds = |time: Option<GoTime>| time.map_or(0.0, |time| from_int(time.unix()));
        let started = seconds(status.and_then(|status| status.started_at));
        let completed = seconds(status.and_then(|status| status.completed_at));
        families.started.set_with(&base, started)?;
        families.completed.set_with(&base, completed)
    }

    /// Deletes every series of a removed service.
    pub fn clear(&self, metadata: &ServiceMetadata) {
        let Some(families) = self.families("clear") else {
            return;
        };
        let base = labels_for_metadata(metadata);
        for state in KNOWN_STATES {
            families
                .state
                .delete_with(&clone_labels_with_state(&base, state));
        }
        families.started.delete_with(&base);
        families.completed.delete_with(&base);
    }
}

#[cfg(test)]
mod tests;
