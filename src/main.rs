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

use service_kit::runtime::{build_runtime, run_until_exit};
use service_kit::signal::lazy_shutdown_signal;
use swarm_scheduler_exporter::run::run_with;

/// Builds the runtime by hand, rather than with `#[tokio::main]`, so its shutdown is bounded by
/// what is left of the shutdown budget: dropping a runtime waits for its blocking tasks, with no
/// bound at all. The shutdown signal is the lazy one: its SIGINT and SIGTERM handlers are installed
/// only once the shutdown watch `run_with` spawns first polls it, after the flags, the
/// `--healthcheck` probe, the logger and the metrics, so a signal before then still terminates
/// the process, as before Go's `signal.NotifyContext`.
fn main() -> ExitCode {
    let runtime = match build_runtime(None) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let code = run_until_exit(runtime, || {
        run_with(
            std::env::args_os(),
            |key| std::env::var(key).ok(),
            lazy_shutdown_signal(),
        )
    });
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
