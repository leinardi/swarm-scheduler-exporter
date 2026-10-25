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

//! Helpers the integration tests share.

pub mod fake_engine;

use std::fmt::{Debug, Display};

/// `unwrap` and `unwrap_err` for the helpers of a test, which clippy's test allowance does not
/// cover: the panic names the caller's line and the error.
pub trait OrPanic<T, E> {
    /// The value, or a panic with the error.
    fn or_panic(self) -> T;
    /// The error, or a panic with the value.
    fn or_panic_err(self) -> E;
}

impl<T: Debug, E: Display> OrPanic<T, E> for Result<T, E> {
    #[track_caller]
    fn or_panic(self) -> T {
        match self {
            Ok(value) => value,
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[track_caller]
    fn or_panic_err(self) -> E {
        match self {
            Ok(value) => panic!("unexpected success: {value:?}"),
            Err(error) => error,
        }
    }
}
