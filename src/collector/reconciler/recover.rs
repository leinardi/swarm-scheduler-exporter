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

//! Go's `recover()` around one reconcile step (Q10): the step's future is polled inside
//! `catch_unwind`, so a panic on unexpected Docker data costs that step, not the process. While a
//! step is polled, a panic hook captures the backtrace for the log record instead of printing
//! the default message to stderr; any other panic still reaches the previous hook.
//!
//! futures' `CatchUnwind` would catch the panic too, but cannot mark the poll as inside a
//! recovered step for the hook, so the wrapper is written here.

use std::any::Any;
use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::Once;
use std::task::{Context, Poll};

thread_local! {
    /// How many recovered steps this thread is polling, nested.
    static RECOVERING: Cell<u32> = const { Cell::new(0) };
    /// The backtrace of the panic the hook saw inside a recovered step.
    static CAPTURED: RefCell<Option<String>> = const { RefCell::new(None) };
}

static HOOK: Once = Once::new();

/// Installs, once per process, the hook that keeps recovered panics quiet and captures their
/// backtrace.
fn install_hook() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if RECOVERING.with(Cell::get) > 0 {
                let stack = Backtrace::force_capture().to_string();
                CAPTURED.with(|captured| *captured.borrow_mut() = Some(stack));
                return;
            }
            previous(info);
        }));
    });
}

/// A recovered panic: Go's `panic` value as a string, and the stack at the panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PanicReport {
    pub(super) payload: String,
    pub(super) stack: String,
}

/// The payload of a panic as Go's `%v` of the recovered value prints a string panic.
fn payload_string(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_owned();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "Box<dyn Any>".to_owned()
}

/// Marks this thread as polling a recovered step until dropped.
struct Recovering;

impl Recovering {
    fn enter() -> Self {
        RECOVERING.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for Recovering {
    fn drop(&mut self) {
        RECOVERING.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// `future`, whose panic becomes an `Err` instead of unwinding further.
struct Recovered<F> {
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Recovered<F> {
    type Output = Result<F::Output, PanicReport>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = &mut self.inner;
        let polled = {
            let _recovering = Recovering::enter();
            panic::catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(context)))
        };
        match polled {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(payload) => Poll::Ready(Err(PanicReport {
                payload: payload_string(payload.as_ref()),
                stack: CAPTURED
                    .with(|captured| captured.borrow_mut().take())
                    .unwrap_or_default(),
            })),
        }
    }
}

/// Runs `future`, returning its panic as a [`PanicReport`]. The step may have stopped halfway;
/// the caller repairs what it left.
pub(super) async fn recover<F: Future>(future: F) -> Result<F::Output, PanicReport> {
    install_hook();
    Recovered {
        inner: Box::pin(future),
    }
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panicking_step_becomes_a_report() {
        let report: Result<(), PanicReport> =
            recover(async { panic!("unexpected service data") }).await;
        let report = report.unwrap_err();
        assert_eq!(report.payload, "unexpected service data");
        assert!(!report.stack.is_empty(), "the hook captured the backtrace");

        let formatted: Result<(), PanicReport> = recover(async { panic!("{} items", 3) }).await;
        assert_eq!(formatted.unwrap_err().payload, "3 items");

        let other: Result<(), PanicReport> = recover(async { std::panic::panic_any(42_u8) }).await;
        assert_eq!(other.unwrap_err().payload, "Box<dyn Any>");
    }

    #[tokio::test]
    async fn a_step_that_returns_keeps_its_value() {
        assert_eq!(recover(async { 7 }).await, Ok(7));
        // Outside a recovered step the thread is not marked.
        assert_eq!(RECOVERING.with(Cell::get), 0);
    }
}
