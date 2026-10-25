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

//! What the Go suite took from the testing package, for an async runner: a test handle
//! ([`TestHandle`]) with Go's `Errorf` (record and go on), `Fatalf` (record and return [`Fatal`]),
//! `Logf`, `Cleanup` (last registered, first run, after the test's context is cancelled), `Failed`
//! and `Run` for subtests, and the suite's shared state: the cluster, the manager's client, the
//! exporter binary and the run filter. Output follows Go's verbose test output: `=== RUN`, the
//! test's log lines, then `--- PASS:` or `--- FAIL:` with the seconds, subtests indented under
//! their parent.

use std::cell::{Cell, RefCell};
use std::fmt::Display;
use std::fmt::Write as _;
use std::future::Future;
use std::io::{self, Write as _};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use bollard::Docker;
use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use testenv::Cluster;
use testenv::context::Context;
use testenv::run_filter::RunFilter;
use tokio::time::Instant;

/// What every test shares: brought up once, before the first test.
pub struct Suite {
    /// The test Swarm.
    pub cluster: Cluster,
    /// A client of the Swarm's manager.
    pub swarm: Docker,
    /// The exporter under test (`SSE_IT_BINARY`).
    pub exporter_binary: PathBuf,
    /// The `--run` filter, which subtests are matched against too.
    pub filter: RunFilter,
    /// Ends at the suite deadline or on SIGINT or SIGTERM; every test's context derives from it.
    pub ctx: Context,
}

static SUITE: OnceLock<Suite> = OnceLock::new();

/// Installs the suite; the runner calls it once, after bring-up.
pub fn install(suite: Suite) {
    assert!(SUITE.set(suite).is_ok(), "the suite is installed once");
}

/// The suite.
pub fn suite() -> &'static Suite {
    SUITE
        .get()
        .unwrap_or_else(|| panic!("the suite is used before bring-up"))
}

/// The test Swarm.
pub fn cluster() -> &'static Cluster {
    &suite().cluster
}

/// A client of the Swarm's manager.
pub fn swarm() -> &'static Docker {
    &suite().swarm
}

/// A test stopped by a fatal failure, which [`TestHandle::fatal`] has already recorded.
#[derive(Debug)]
pub struct Fatal;

/// What a test or subtest returns.
pub type TestResult = Result<(), Fatal>;

/// Turns a failed result into a fatal test failure, its error as the message.
pub trait OrFatal<V> {
    /// The value, or [`Fatal`] with the error recorded on `test`.
    fn or_fatal(self, test: &TestHandle) -> Result<V, Fatal>;
}

impl<V, E: Display> OrFatal<V> for Result<V, E> {
    fn or_fatal(self, test: &TestHandle) -> Result<V, Fatal> {
        self.map_err(|error| test.fatal(error))
    }
}

type Cleanup = Box<dyn FnOnce() -> LocalBoxFuture<'static, ()>>;

struct Inner {
    path: Vec<String>,
    ctx: Context,
    failed: Cell<bool>,
    skipped: Cell<bool>,
    cleanups: RefCell<Vec<Cleanup>>,
}

/// A running test or subtest. Clones share it.
#[derive(Clone)]
pub struct TestHandle(Rc<Inner>);

impl TestHandle {
    fn new(path: Vec<String>, ctx: Context) -> Self {
        Self(Rc::new(Inner {
            path,
            ctx,
            failed: Cell::new(false),
            skipped: Cell::new(false),
            cleanups: RefCell::new(Vec::new()),
        }))
    }

    /// The full name: `TestX`, or `TestX/subtest`.
    pub fn name(&self) -> String {
        self.0.path.join("/")
    }

    fn indent(&self) -> String {
        "    ".repeat(self.0.path.len().saturating_sub(1))
    }

    /// Go's `testCtx(test)`: ends with the test, or at the suite deadline, whichever comes first.
    pub fn ctx(&self) -> &Context {
        &self.0.ctx
    }

    /// `test.Logf`: printed now, under the test's name.
    pub fn log(&self, message: impl Display) {
        let indent = format!("{}    ", self.indent());
        let text = message.to_string();
        let mut lines = text.lines();
        let mut out = String::new();
        if let Some(first) = lines.next() {
            let _ = writeln!(out, "{indent}{first}");
        }
        for line in lines {
            let _ = writeln!(out, "{indent}    {line}");
        }
        say_raw(&out);
    }

    /// `test.Errorf`: records a failure and lets the test go on.
    pub fn error(&self, message: impl Display) {
        self.log(message);
        self.0.failed.set(true);
    }

    /// `test.Fatalf`: records a failure; return the [`Fatal`] to stop the test.
    pub fn fatal(&self, message: impl Display) -> Fatal {
        self.error(message);
        Fatal
    }

    /// `test.Skipf`: logs why, and marks the test skipped unless it has already failed; return
    /// the [`Fatal`] to stop it.
    pub fn skip(&self, message: impl Display) -> Fatal {
        self.log(message);
        self.0.skipped.set(true);
        Fatal
    }

    /// `test.Failed`.
    pub fn failed(&self) -> bool {
        self.0.failed.get()
    }

    /// `test.Cleanup`: `cleanup` runs when the test ends, after its context is cancelled, the last
    /// registered first.
    pub fn cleanup<F, Fut>(&self, cleanup: F)
    where
        F: FnOnce() -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        self.0
            .cleanups
            .borrow_mut()
            .push(Box::new(move || Box::pin(cleanup())));
    }

    /// `test.Run`: runs `body` as the subtest `name`, unless the run filter leaves it out, and
    /// returns whether it passed. A failed subtest fails its parent.
    pub async fn run<F, Fut>(&self, name: &str, body: F) -> bool
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = TestResult>,
    {
        let mut path = self.0.path.clone();
        path.push(name.to_owned());
        let names: Vec<&str> = path.iter().map(String::as_str).collect();
        if !suite().filter.matches(&names) {
            return true;
        }
        let sub = Self::new(path, self.0.ctx.with_cancel());
        execute(&sub, body(sub.clone())).await;
        if sub.failed() {
            self.0.failed.set(true);
        }
        !sub.failed()
    }
}

/// Runs the top-level test `name` with `body`, and returns whether it passed.
pub async fn run_test<F, Fut>(name: &str, ctx: &Context, body: F) -> bool
where
    F: FnOnce(TestHandle) -> Fut,
    Fut: Future<Output = TestResult>,
{
    let test = TestHandle::new(vec![name.to_owned()], ctx.with_cancel());
    execute(&test, body(test.clone())).await;
    !test.failed()
}

/// Runs a test body, a panic counting as a failure, then its cleanups, then prints its result.
async fn execute(test: &TestHandle, body: impl Future<Output = TestResult>) {
    say_raw(&format!("{}=== RUN   {}\n", test.indent(), test.name()));
    let started = Instant::now();
    if let Err(panic) = AssertUnwindSafe(body).catch_unwind().await {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        test.error(format!("panic: {message}"));
    }
    test.0.ctx.cancel();
    loop {
        let next = test.0.cleanups.borrow_mut().pop();
        let Some(cleanup) = next else {
            break;
        };
        cleanup().await;
    }
    let verdict = if test.failed() {
        "FAIL"
    } else if test.0.skipped.get() {
        "SKIP"
    } else {
        "PASS"
    };
    say_raw(&format!(
        "{}--- {verdict}: {} ({:.2}s)\n",
        test.indent(),
        test.name(),
        started.elapsed().as_secs_f64()
    ));
}

/// One line to stderr, where the harness's log goes too.
pub fn say(line: &str) {
    say_raw(&format!("{line}\n"));
}

fn say_raw(text: &str) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}
