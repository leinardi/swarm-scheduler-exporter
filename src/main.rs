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

//! The swarm-scheduler-exporter binary: runs the exporter on a runtime of its own and returns
//! its exit code.

use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use swarm_scheduler_exporter::run::{run_with, shutdown_signal};

/// Builds the runtime by hand, rather than with `#[tokio::main]`, so its shutdown is bounded by
/// what is left of the shutdown budget: dropping a runtime waits for its blocking tasks, with no
/// bound at all.
fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let exit = runtime.block_on(run_with(
        std::env::args_os(),
        |key| std::env::var(key).ok(),
        shutdown_signal(),
    ));
    let remaining = exit.deadline.map_or(Duration::ZERO, |deadline| {
        deadline.saturating_duration_since(tokio::time::Instant::now())
    });
    runtime.shutdown_timeout(remaining);
    ExitCode::from(u8::try_from(exit.code).unwrap_or(1))
}
