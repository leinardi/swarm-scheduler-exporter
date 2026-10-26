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

//! Where each HTTP/1 request ends in the bytes a connection read, so the deadlines can tell a
//! request already started from an idle connection.
//!
//! hyper reads ahead: one read can carry a whole request and the first bytes of the next one,
//! which hyper keeps in its own buffer. Go's server checks for such bytes before it arms the idle
//! deadline (`bufr.Peek` returns at once) and gives the next request its header deadline from the
//! end of the previous response. The I/O wrapper cannot see hyper's buffer, so it follows the
//! framing itself: the head up to its empty line, then a `Content-Length` or chunked body. It only
//! counts messages; parsing and rejecting them stays hyper's, and a message hyper rejects ends the
//! connection, so a framing this tracker gets wrong on malformed input has no later request to
//! affect.

/// Longest header line kept for inspection: `Content-Length` and `Transfer-Encoding` fit well
/// within it, and anything past it is dropped rather than buffered.
const MAX_LINE: usize = 1024;

#[derive(Debug)]
enum State {
    /// Between messages: the next byte starts one.
    Between,
    /// In a head; `request_line` until its first line ends.
    Head {
        line: Vec<u8>,
        request_line: bool,
        content_length: u64,
        chunked: bool,
    },
    /// In a `Content-Length` body, with this many bytes left.
    Body(u64),
    /// In a chunk-size line.
    ChunkSize(Vec<u8>),
    /// In a chunk's data, with this many bytes left.
    ChunkData(u64),
    /// After a chunk's data, up to the end of its line.
    ChunkEnd,
    /// In the trailer section after the last chunk; whether the current line is empty so far.
    Trailer(bool),
}

/// The request framing of one connection's inbound bytes.
#[derive(Debug)]
pub struct Framer {
    state: State,
    started: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    /// A connection that read nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Between,
            started: 0,
        }
    }

    /// How many messages have read at least one byte.
    #[must_use]
    pub fn started(&self) -> u64 {
        self.started
    }

    /// Follows `bytes`, the next ones the connection read.
    pub fn feed(&mut self, mut bytes: &[u8]) {
        while let Some((&byte, rest)) = bytes.split_first() {
            match &mut self.state {
                State::Body(left) | State::ChunkData(left) => {
                    let available = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    let taken = (*left).min(available);
                    *left -= taken;
                    bytes = &bytes[usize::try_from(taken).unwrap_or(bytes.len())..];
                    if *left == 0 {
                        self.state = match self.state {
                            State::ChunkData(_) => State::ChunkEnd,
                            State::Between
                            | State::Head { .. }
                            | State::Body(_)
                            | State::ChunkSize(_)
                            | State::ChunkEnd
                            | State::Trailer(_) => State::Between,
                        };
                    }
                    continue;
                }
                State::Between => {
                    self.started += 1;
                    self.state = State::Head {
                        line: Vec::new(),
                        request_line: true,
                        content_length: 0,
                        chunked: false,
                    };
                    continue;
                }
                State::Head {
                    line,
                    request_line,
                    content_length,
                    chunked,
                } => {
                    if byte != b'\n' {
                        if line.len() < MAX_LINE {
                            line.push(byte);
                        }
                    } else if *request_line {
                        *request_line = false;
                        line.clear();
                    } else if trim_cr(line).is_empty() {
                        self.state = if *chunked {
                            State::ChunkSize(Vec::new())
                        } else if *content_length > 0 {
                            State::Body(*content_length)
                        } else {
                            State::Between
                        };
                    } else {
                        header(trim_cr(line), content_length, chunked);
                        line.clear();
                    }
                }
                State::ChunkSize(line) => {
                    if byte == b'\n' {
                        self.state = match chunk_size(trim_cr(line)) {
                            Some(0) => State::Trailer(true),
                            Some(size) => State::ChunkData(size),
                            // hyper rejects the message and ends the connection.
                            None => State::Between,
                        };
                    } else if line.len() < MAX_LINE {
                        line.push(byte);
                    }
                }
                State::ChunkEnd => {
                    if byte == b'\n' {
                        self.state = State::ChunkSize(Vec::new());
                    }
                }
                State::Trailer(empty) => {
                    if byte == b'\n' {
                        if *empty {
                            self.state = State::Between;
                        } else {
                            *empty = true;
                        }
                    } else if byte != b'\r' {
                        *empty = false;
                    }
                }
            }
            bytes = rest;
        }
    }
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Records what a header line says about the body: its `Content-Length`, or whether its last
/// transfer coding is `chunked`, which takes precedence as it does for hyper.
fn header(line: &[u8], content_length: &mut u64, chunked: &mut bool) {
    let Some(colon) = line.iter().position(|&byte| byte == b':') else {
        return;
    };
    let (name, value) = (line[..colon].trim_ascii(), line[colon + 1..].trim_ascii());
    if name.eq_ignore_ascii_case(b"content-length") {
        if let Some(length) = std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse().ok())
        {
            *content_length = length;
        }
    } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
        *chunked = value
            .rsplit(|&byte| byte == b',')
            .next()
            .is_some_and(|coding| coding.trim_ascii().eq_ignore_ascii_case(b"chunked"));
    }
}

/// A chunk-size line's size, without its extensions.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let size = line
        .split(|&byte| byte == b';')
        .next()
        .unwrap_or_default()
        .trim_ascii();
    std::str::from_utf8(size)
        .ok()
        .and_then(|size| u64::from_str_radix(size, 16).ok())
}

#[cfg(test)]
mod tests;
