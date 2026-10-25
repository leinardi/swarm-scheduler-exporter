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

//! Go's `TestSplitHostPort` and `TestSplitHostPortErrors` cases, from the net package's tests.

use super::*;

/// The well-formed cases of Go's `splitJoinTests`.
#[test]
fn splits_as_go() {
    for (host_port, host, port) in [
        ("www.google.com:80", "www.google.com", "80"),
        ("127.0.0.1:1234", "127.0.0.1", "1234"),
        ("[::1]:80", "::1", "80"),
        ("[fe80::1%lo0]:80", "fe80::1%lo0", "80"),
        ("localhost%lo0:80", "localhost%lo0", "80"),
        (":8888", "", "8888"),
        ("[::]:8888", "::", "8888"),
        ("host:", "host", ""),
        (":", "", ""),
        ("[]:", "", ""),
    ] {
        assert_eq!(split_host_port(host_port), Ok((host, port)), "{host_port}");
    }
}

/// Go's `splitFailureTests`, with their messages.
#[test]
fn fails_as_go() {
    for (host_port, message) in [
        ("www.google.com", "missing port in address"),
        ("127.0.0.1", "missing port in address"),
        ("[::1]", "missing port in address"),
        ("[fe80::1%lo0]", "missing port in address"),
        ("[localhost%lo0]", "missing port in address"),
        ("localhost%lo0", "missing port in address"),
        ("::1", "too many colons in address"),
        ("fe80::1%lo0", "too many colons in address"),
        ("fe80::1%lo0:80", "too many colons in address"),
        ("[foo:bar]", "missing port in address"),
        ("[foo:bar]baz", "missing port in address"),
        ("[foo]bar:baz", "missing port in address"),
        ("[foo]:[bar]:baz", "too many colons in address"),
        ("[foo]:[bar]baz", "unexpected '[' in address"),
        ("foo[bar]:baz", "unexpected '[' in address"),
        ("foo]bar:baz", "unexpected ']' in address"),
        ("[::1:80", "missing ']' in address"),
    ] {
        let error = split_host_port(host_port).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("address {host_port}: {message}"),
            "{host_port}"
        );
    }
    assert_eq!(
        split_host_port("").unwrap_err().to_string(),
        "missing port in address"
    );
}

#[test]
fn joins_as_go() {
    assert_eq!(join_host_port("127.0.0.1", "80"), "127.0.0.1:80");
    assert_eq!(join_host_port("::1", "80"), "[::1]:80");
    assert_eq!(join_host_port("", "80"), ":80");
}
