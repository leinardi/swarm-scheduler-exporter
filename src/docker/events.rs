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

//! The event stream: a port of moby's `NewJSONStreamDecoder` with Go's `json.Decoder` stream
//! semantics, over a response body that arrives in chunks.
//!
//! The decoder is picked by the response `Content-Type`, compared exactly as moby compares it:
//! `application/json-seq` goes through an RS filter that drops every 0x1E byte, wherever the
//! chunks split (moby's `rsFilterReader`; a raw 0x1E cannot occur inside a JSON string), and
//! everything else (`application/json`, `application/x-ndjson`, `application/jsonl`, a missing or
//! unknown type) is read as a plain sequence of JSON values.
//!
//! How the stream ends is what the listener's log level depends on: a body that ends between two
//! values, with only whitespace (or RS bytes) left after the last one, is Go's `io.EOF`, a clean
//! close; a body that ends inside a value, or fails (a connection cut before the terminating
//! chunk, a transport error), is Go's `io.ErrUnexpectedEOF`. The failure is detected from the body
//! returning an error, not from hyper's `is_incomplete_message`, which is false for a chunked body
//! cut by the peer.

use std::fmt;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use serde::Deserialize;

use super::error::Error;

/// moby's `MediaTypeJSONSequence`.
pub const MEDIA_TYPE_JSON_SEQUENCE: &str = "application/json-seq";

/// The `Accept` lines of the events request, in moby's order, each its own header line.
pub const EVENTS_ACCEPT: [&str; 3] = [
    "application/jsonl",
    "application/x-ndjson;q=0.9",
    "application/json-seq;q=0.5",
];

/// The record separator of RFC 7464 JSON text sequences.
const RS: u8 = 0x1E;

/// How a stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// Go's `io.EOF`: the body ended cleanly between two values.
    Eof,
    /// Go's `io.ErrUnexpectedEOF`: the body ended inside a value, or failed.
    UnexpectedEof,
    /// A value that is not JSON, or not an event.
    Decode,
    /// The source of the events went away without saying why: Go's `ErrEventsStreamClosed`, the
    /// closed channel of a test double.
    Closed,
}

/// Why a stream ended; every stream ends with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    kind: StreamErrorKind,
    message: String,
}

impl StreamError {
    /// A stream that ended with `kind`, worded as `message`.
    #[must_use]
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Go's `io.EOF`.
    #[must_use]
    pub fn eof() -> Self {
        Self::new(StreamErrorKind::Eof, "EOF")
    }

    /// Go's `io.ErrUnexpectedEOF`.
    #[must_use]
    pub fn unexpected_eof() -> Self {
        Self::new(StreamErrorKind::UnexpectedEof, "unexpected EOF")
    }

    /// How the stream ended.
    #[must_use]
    pub fn kind(&self) -> StreamErrorKind {
        self.kind
    }

    /// Whether it is Go's `errors.Is(err, io.EOF)`: a clean close, logged at info.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.kind == StreamErrorKind::Eof
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StreamError {}

/// A sequence of JSON values read from a body, as Go's `json.Decoder` reads them.
#[derive(Debug)]
pub struct JsonStream<B> {
    body: B,
    buffer: Vec<u8>,
    rs_filter: bool,
    ended: bool,
}

impl<B> JsonStream<B>
where
    B: hyper::body::Body<Data = Bytes, Error = Error> + Unpin,
{
    /// A stream over `body`, whose response carried `content_type`.
    pub fn new(body: B, content_type: Option<&str>) -> Self {
        Self {
            body,
            buffer: Vec::new(),
            rs_filter: content_type == Some(MEDIA_TYPE_JSON_SEQUENCE),
            ended: false,
        }
    }

    /// Whether the body goes through the RS filter.
    #[must_use]
    pub fn filters_rs(&self) -> bool {
        self.rs_filter
    }

    /// The next value; a `null` is the zero value, as Go decodes it.
    ///
    /// # Errors
    ///
    /// [`StreamError`] once the stream is over, with how it ended.
    pub async fn next<T>(&mut self) -> Result<T, StreamError>
    where
        T: Default + for<'de> Deserialize<'de>,
    {
        loop {
            let start = self
                .buffer
                .iter()
                .position(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'));
            if let Some(start) = start {
                let mut values = serde_json::Deserializer::from_slice(&self.buffer[start..])
                    .into_iter::<Option<T>>();
                match values.next() {
                    Some(Ok(value)) => {
                        let consumed = start + values.byte_offset();
                        self.buffer.drain(..consumed);
                        return Ok(value.unwrap_or_default());
                    }
                    Some(Err(error)) if error.is_eof() => {
                        if self.ended {
                            return Err(StreamError::unexpected_eof());
                        }
                    }
                    Some(Err(error)) => {
                        return Err(StreamError::new(StreamErrorKind::Decode, error.to_string()));
                    }
                    None => {}
                }
            } else {
                self.buffer.clear();
                if self.ended {
                    return Err(StreamError::eof());
                }
            }
            self.fill().await?;
        }
    }

    /// Reads the next chunk into the buffer, or marks the body as ended.
    async fn fill(&mut self) -> Result<(), StreamError> {
        match self.body.frame().await {
            None => self.ended = true,
            // Go's chunked reader reports a cut body as io.ErrUnexpectedEOF, and the decoder
            // returns any read error as it is: either way not io.EOF.
            Some(Err(error)) => {
                return Err(StreamError::new(
                    StreamErrorKind::UnexpectedEof,
                    error.to_string(),
                ));
            }
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    if self.rs_filter {
                        self.buffer
                            .extend(data.iter().copied().filter(|byte| *byte != RS));
                    } else {
                        self.buffer.extend_from_slice(&data);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
