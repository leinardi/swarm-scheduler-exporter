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

//! What the Go suite took from the testing package is testenv-core's `testing` module: the test
//! handle ([`TestHandle`]) with Go's `Errorf`, `Fatalf`, `Skipf`, `Logf`, `Cleanup`, `Failed` and
//! `Run`, and the verbose output (`=== RUN`, the test's log lines, then `--- PASS:`, `--- FAIL:`
//! or `--- SKIP:` with the seconds, subtests indented under their parent). What is this suite's
//! own is here: its shared state, the cluster, the manager's client, the exporter binary and the
//! suite context.

use std::path::PathBuf;

use bollard::Docker;
use testenv::Cluster;
use testenv_core::context::Context;
use testenv_core::testing;
pub use testenv_core::testing::{Fatal, OrFatal, TestHandle, TestResult};

/// What every test shares: brought up once, before the first test.
pub struct Suite {
    /// The test Swarm.
    pub cluster: Cluster,
    /// A client of the Swarm's manager.
    pub swarm: Docker,
    /// The exporter under test (`SSE_IT_BINARY`).
    pub exporter_binary: PathBuf,
    /// Ends at the suite deadline or on SIGINT or SIGTERM; every test's context derives from it.
    pub ctx: Context,
}

static SUITE: testing::Suite<Suite> = testing::Suite::new("the suite is used before bring-up");

/// Installs the suite; the runner calls it once, after bring-up.
pub fn install(suite: Suite) {
    SUITE.install(suite);
}

/// The suite.
pub fn suite() -> &'static Suite {
    SUITE.get()
}

/// The test Swarm.
pub fn cluster() -> &'static Cluster {
    &suite().cluster
}

/// A client of the Swarm's manager.
pub fn swarm() -> &'static Docker {
    &suite().swarm
}
