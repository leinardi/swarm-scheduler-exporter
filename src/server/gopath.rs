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

//! The parts of Go's `net/url` and of the Go 1.22+ `net/http.ServeMux` that decide, for a request
//! target, between serving a route, a 307 to the cleaned path and a 404: `url.unescape`,
//! `url.escape`, `shouldEscape`, `validEncoded`, `setPath` and `EscapedPath`; the mux's
//! `cleanPath`, its per-segment unescaped match and `Redirect`'s `htmlEscape`; `path.Clean`; and
//! `url.ParseQuery`, which promhttp reads `name[]` with.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// How Go's mux routes a request target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// An exact match of a registered pattern: the pattern.
    Matched(&'static str),
    /// `RedirectHandler(location, StatusTemporaryRedirect)`: the path was not clean.
    Redirect(String),
    /// `NotFoundHandler()`.
    NotFound,
    /// `url.ParseRequestURI` failed: Go answers `400 Bad Request` before the mux runs.
    BadRequest,
}

/// The escaping modes of `url.shouldEscape` the exporter needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Path,
    Host,
}

/// Routes `raw_path`, the request target's path as on the wire, with `raw_query`, against
/// `patterns`, each an exact one-segment path such as `/metrics`.
#[must_use]
pub fn route(raw_path: &str, raw_query: Option<&str>, patterns: &[&'static str]) -> Route {
    // url.setPath: Path is the unescaped form; RawPath keeps the wire form when it differs from
    // the default encoding.
    let Some(path) = unescape(raw_path) else {
        return Route::BadRequest;
    };
    let raw = if escape(&path, Mode::Path) == raw_path {
        ""
    } else {
        raw_path
    };
    let escaped = escaped_path(&path, raw);
    let cleaned = clean_path(&escaped);
    if cleaned != escaped {
        // findHandler: url.URL{Path: path, RawQuery: r.URL.RawQuery}.String(). The path set is
        // the cleaned *escaped* path, so String() escapes it again (a `%` becomes `%25`), and
        // Redirect's own path.Clean of it changes nothing.
        let mut location = escape(cleaned.as_bytes(), Mode::Path);
        if let Some(query) = raw_query.filter(|query| !query.is_empty()) {
            location.push('?');
            location.push_str(query);
        }
        return Route::Redirect(location);
    }
    patterns
        .iter()
        .copied()
        .find(|pattern| matches_exact(&cleaned, pattern))
        .map_or(Route::NotFound, Route::Matched)
}

/// The routing tree's match of a literal one-segment pattern: the cleaned path is `/` and one
/// segment, and that segment, unescaped, is the pattern's.
fn matches_exact(cleaned: &str, pattern: &str) -> bool {
    let Some(segment) = cleaned.strip_prefix('/') else {
        return false;
    };
    if segment.contains('/') {
        return false;
    }
    let unescaped = unescape(segment).unwrap_or_else(|| segment.as_bytes().to_vec());
    pattern.strip_prefix('/').map(str::as_bytes) == Some(unescaped.as_slice())
}

/// `url.URL.EscapedPath`.
fn escaped_path(path: &[u8], raw_path: &str) -> String {
    if !raw_path.is_empty()
        && valid_encoded(raw_path)
        && unescape(raw_path).as_deref() == Some(path)
    {
        return raw_path.to_owned();
    }
    if path == b"*" {
        return "*".to_owned();
    }
    escape(path, Mode::Path)
}

/// The mux's `cleanPath`: rooted, cleaned, and with a trailing slash kept.
#[must_use]
pub fn clean_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    let rooted = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let mut cleaned = path_clean(&rooted);
    if rooted.ends_with('/') && cleaned != "/" {
        cleaned.push('/');
    }
    cleaned
}

/// `path.Clean`: `//` collapsed, `.` dropped, `..` resolved, and no trailing slash but the root's.
#[must_use]
pub fn path_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let rooted = path.starts_with('/');
    let mut elements: Vec<&str> = Vec::new();
    for element in path.split('/') {
        match element {
            "" | "." => {}
            ".." => {
                if elements.last().is_some_and(|last| *last != "..") {
                    elements.pop();
                } else if !rooted {
                    elements.push("..");
                }
            }
            other => elements.push(other),
        }
    }
    let joined = elements.join("/");
    if rooted {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

/// `url.unescape` in path mode: every `%` takes two hex digits, and `+` stays `+`. The result is
/// bytes, as a Go string holds any.
fn unescape(text: &str) -> Option<Vec<u8>> {
    unescape_with(text, false)
}

/// `url.unescape`, with `+` read as a space in query mode.
fn unescape_with(text: &str, plus_is_space: bool) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = bytes.get(index + 1).copied().and_then(hex_value)?;
                let low = bytes.get(index + 2).copied().and_then(hex_value)?;
                out.push((high << 4) | low);
                index += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `url.shouldEscape` in `mode`.
fn should_escape(byte: u8, mode: Mode) -> bool {
    if byte.is_ascii_alphanumeric() {
        return false;
    }
    if mode == Mode::Host
        && matches!(
            byte,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'['
                | b']'
                | b'<'
                | b'>'
                | b'"'
        )
    {
        return false;
    }
    match byte {
        b'-' | b'_' | b'.' | b'~' => false,
        // In a path only `?` of these is escaped; in a host every one left here is.
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => {
            mode == Mode::Host || byte == b'?'
        }
        _ => true,
    }
}

/// `url.escape` in `mode`, with uppercase hex.
fn escape(bytes: &[u8], mode: Mode) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        if should_escape(byte, mode) {
            let _ = write!(out, "%{byte:02X}");
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

/// `url.escape(host, encodeHost)`, as `URL.String` writes the host.
#[must_use]
pub fn escape_host(host: &str) -> String {
    escape(host.as_bytes(), Mode::Host)
}

/// `url.validEncoded` in path mode.
fn valid_encoded(text: &str) -> bool {
    text.bytes().all(|byte| {
        matches!(
            byte,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'['
                | b']'
                | b'%'
        ) || !should_escape(byte, Mode::Path)
    })
}

/// The mux's `htmlEscape`, for `Redirect`'s body.
#[must_use]
pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// `url.ParseQuery`, ignoring its error as `URL.Query` does: `&`-separated pairs, a pair with a
/// `;` or a bad escape skipped, keys and values unescaped with `+` as a space. Keys and values
/// are bytes, as Go strings hold any.
#[must_use]
pub fn parse_query(query: &str) -> BTreeMap<Vec<u8>, Vec<Vec<u8>>> {
    let mut values: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() || pair.contains(';') {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(key), Some(value)) = (unescape_with(key, true), unescape_with(value, true))
        else {
            continue;
        };
        values.entry(key).or_default().push(value);
    }
    values
}

#[cfg(test)]
mod tests;
