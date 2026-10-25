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

//! What a poll counts tasks against: the services and nodes of one moment, copied out of the
//! caches so the poller never reads them while the reconciler writes. The reconciler builds it
//! and judges the counts against it (plan phase 14); it is immutable once built.

use std::collections::{BTreeMap, HashMap};

use crate::docker::types::Node;

use super::desired::labels_for_metadata;
use super::types::ServiceMetadata;

/// One service of a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct PollService {
    /// The reconciler's generation of the service when the snapshot was taken.
    pub generation: u64,
    /// The service's metadata.
    pub metadata: ServiceMetadata,
    /// Its label identity, [`labels_for_metadata`] of the metadata.
    pub labels: BTreeMap<String, String>,
}

impl PollService {
    /// The service at `generation` with `metadata`, and the labels it implies.
    #[must_use]
    pub fn new(generation: u64, metadata: ServiceMetadata) -> Self {
        Self {
            generation,
            labels: labels_for_metadata(&metadata),
            metadata,
        }
    }
}

/// A polling snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollSnapshot {
    /// The services, by ID.
    pub services: HashMap<String, PollService>,
    /// The node snapshot.
    pub nodes: Vec<Node>,
    /// The reconciler's node generation when the snapshot was taken.
    pub node_generation: u64,
    /// The reconciler's resync epoch when the snapshot was taken.
    pub epoch: u64,
}

impl PollSnapshot {
    /// The service IDs, sorted, so iterating them is deterministic.
    #[must_use]
    pub fn service_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.services.keys().cloned().collect();
        ids.sort_unstable();
        ids
    }
}
