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

//! Restart-policy metadata, published whole once per reconciler cycle.

use std::sync::Arc;

use prometheus_client::registry::Registry;
use tracing::error;

use super::metrics_ids::{LABEL_DISPLAY_NAME, LABEL_SERVICE, LABEL_SERVICE_MODE, LABEL_STACK};
use super::types::{RestartCondition, ServiceMetadata, display_name};
use crate::metrics::{self, Desc, SnapshotBuilder, SnapshotFamily};

const LABEL_RESTART_CONDITION: &str = "restart_condition";
const CONDITIONS: [(&str, RestartCondition); 3] = [
    ("none", RestartCondition::None),
    ("on-failure", RestartCondition::OnFailure),
    ("any", RestartCondition::Any),
];

/// A fixed-label snapshot; custom labels must not reserve `restart_condition` globally.
#[derive(Debug)]
pub struct ServiceRestartPolicy {
    family: Arc<SnapshotFamily>,
}

impl Default for ServiceRestartPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceRestartPolicy {
    /// An unpublished family.
    #[must_use]
    pub fn new() -> Self {
        Self {
            family: Arc::new(SnapshotFamily::new(Desc::new(
                "swarm_service_restart_policy",
                "Swarm service restart condition (one-hot).",
                &[
                    LABEL_STACK,
                    LABEL_SERVICE,
                    LABEL_SERVICE_MODE,
                    LABEL_DISPLAY_NAME,
                    LABEL_RESTART_CONDITION,
                ],
            ))),
        }
    }

    /// Registers the family.
    pub fn register(&self, registry: &mut Registry) {
        metrics::register(registry, &self.family);
    }

    /// Replaces the complete cached service view; a failed build keeps the previous snapshot.
    pub fn publish_from<'a>(&self, services: impl Iterator<Item = &'a ServiceMetadata>) {
        match build_restart_policy(&self.family, services).build() {
            Ok(families) => self.family.collector().publish(families),
            Err(error) => error!(%error, "build restart policy snapshot; keeping previous"),
        }
    }
}

/// Builds exhaustive policy series without publishing them or carrying custom labels.
#[must_use]
pub fn build_restart_policy<'a>(
    family: &SnapshotFamily,
    services: impl Iterator<Item = &'a ServiceMetadata>,
) -> SnapshotBuilder {
    let mut builder = SnapshotBuilder::new();
    for service in services {
        let display = display_name(&service.stack, &service.service);
        // Unknown wire enums fail decoding; this only defends against constructed metadata.
        let effective = if service.restart_condition == RestartCondition::Unrecognized {
            RestartCondition::Any
        } else {
            service.restart_condition
        };
        for (condition, value) in CONDITIONS {
            builder.set(
                family.desc(),
                &[
                    (LABEL_STACK, service.stack.as_str()),
                    (LABEL_SERVICE, service.service.as_str()),
                    (LABEL_SERVICE_MODE, service.service_mode.as_str()),
                    (LABEL_DISPLAY_NAME, display.as_str()),
                    (LABEL_RESTART_CONDITION, condition),
                ],
                f64::from(effective == value),
            );
        }
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::gather_series;
    use crate::test_support::openmetrics::series_id;

    #[test]
    fn policies_are_exhaustive_fixed_label_snapshots() {
        let metrics = ServiceRestartPolicy::new();
        let mut service = ServiceMetadata {
            stack: "stack".to_owned(),
            service: "job".to_owned(),
            service_mode: "replicated".to_owned(),
            custom_labels: [("restart_condition".to_owned(), "custom".to_owned())].into(),
            ..ServiceMetadata::default()
        };
        for condition in [
            RestartCondition::Any,
            RestartCondition::None,
            RestartCondition::OnFailure,
            RestartCondition::Unrecognized,
        ] {
            service.restart_condition = condition;
            let builder = build_restart_policy(&metrics.family, std::iter::once(&service));
            let before = gather_series(&metrics.family);
            let built = builder.build().unwrap();
            assert_eq!(gather_series(&metrics.family), before);
            metrics.family.collector().publish(built);
            let view = gather_series(&metrics.family);
            assert_eq!(view.len(), 3);
            for (label, value) in CONDITIONS {
                assert_eq!(
                    view.get(&series_id(
                        "swarm_service_restart_policy",
                        &[
                            (LABEL_STACK, "stack"),
                            (LABEL_SERVICE, "job"),
                            (LABEL_SERVICE_MODE, "replicated"),
                            (LABEL_DISPLAY_NAME, "stack job"),
                            (LABEL_RESTART_CONDITION, label),
                        ]
                    )),
                    Some(&f64::from(
                        condition == value
                            || (condition == RestartCondition::Unrecognized
                                && value == RestartCondition::Any)
                    ))
                );
            }
        }
        metrics.publish_from(std::iter::empty());
        assert!(gather_series(&metrics.family).is_empty());
    }
}
