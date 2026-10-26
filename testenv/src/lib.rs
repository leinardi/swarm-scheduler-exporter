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

//! The Docker-in-Docker Swarm test harness: a throwaway multi-node Swarm for the integration
//! suite, one manager and N workers, each a privileged docker:dind container on the host daemon,
//! joined over a dedicated bridge network. Everything it creates carries the envid label, so a run
//! can be torn down (or swept after a crash) without touching anything else on the host, including
//! a host that is itself a Swarm node.
//!
//! The harness mutates Docker on purpose: it creates and removes containers, networks and a
//! Swarm. That is why the modules that reach a daemon sit behind the `integration` feature, as
//! Go's sat behind its integration build tag, and why the exporter package depends on this crate
//! in no table. The sweep rules, the redacting logger and the run filter need no daemon and always
//! compile, so their tests run in a plain `cargo test`; so does the reader of the exporter's
//! `/metrics` text the suite scrapes.

#[cfg(feature = "integration")]
mod cluster;
#[cfg(feature = "integration")]
pub mod context;
#[cfg(feature = "integration")]
mod dind;
#[cfg(feature = "integration")]
pub mod envid;
mod error;
#[cfg(feature = "integration")]
mod images;
pub mod metrics_text;
pub mod redact;
pub mod run_filter;
pub mod sweep_rules;

#[cfg(feature = "integration")]
pub use cluster::{
    Cluster, ENV_BINARY, ENV_DIND_IMAGE, ENV_KEEP_ON_FAILURE, ENV_WORKERS, Node, NodeRole, Spec,
    new_logger, spec_from_env,
};
#[cfg(feature = "integration")]
pub use dind::{cleanup_command, up};
pub use error::Error;
#[cfg(feature = "integration")]
pub use images::WORKLOAD_IMAGE;
