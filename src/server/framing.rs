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
//! framing itself, byte by byte and with hyper's rules: empty lines before a request skipped, the
//! head up to its empty line, then a `Content-Length` or chunked body. Every header line is read
//! whole, however long, without buffering it: a value hyper accepts must frame the same here, or
//! the count drifts and a later request gets the wrong deadline. It only counts messages; parsing
//! and rejecting them stays hyper's, and a message hyper rejects ends the connection, so a
//! framing this tracker gets wrong on malformed input has no later request to affect.

const CONTENT_LENGTH: &[u8] = b"content-length";
const TRANSFER_ENCODING: &[u8] = b"transfer-encoding";
const CHUNKED: &[u8] = b"chunked";

#[derive(Debug)]
enum State {
    /// Between messages: the next byte other than an empty line starts one.
    Between,
    /// In a head.
    Head(Head),
    /// In a `Content-Length` body, with this many bytes left.
    Body(u64),
    /// In a chunk-size line: the size so far, and whether its digits ended.
    ChunkSize { size: u64, digits_ended: bool },
    /// In a chunk's data, with this many bytes left.
    ChunkData(u64),
    /// After a chunk's data, up to the end of its line.
    ChunkEnd,
    /// In the trailer section after the last chunk; whether the current line is empty so far.
    Trailer(bool),
}

/// What a head says about its body, so far.
#[derive(Debug)]
struct Head {
    /// Still in the request line.
    request_line: bool,
    /// The current line has nothing but a carriage return so far.
    line_empty: bool,
    field: Field,
    content_length: u64,
    /// Whether the last `Transfer-Encoding` line ends in `chunked`; `None` without one. With one,
    /// hyper ignores every `Content-Length`.
    chunked: Option<bool>,
}

/// The header line being read.
#[derive(Debug)]
enum Field {
    /// Its name so far, lower case; dropped (`Other`) once longer than any name that frames.
    Name(Vec<u8>),
    ContentLength(Length),
    TransferEncoding(Coding),
    /// A field that does not frame the body, or a name past the longest that does.
    Other,
}

/// A `Content-Length` value as hyper reads it: comma-separated, each item trimmed, all equal.
#[derive(Debug, Default)]
struct Length {
    value: LengthValue,
    item: Item,
}

/// The value of the `Content-Length` items so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum LengthValue {
    #[default]
    NoItem,
    Valid(u64),
    /// An item did not parse, or differed from another.
    Invalid,
}

/// One item of a `Content-Length` value.
#[derive(Debug, Default)]
enum Item {
    /// Whitespace only, so far.
    #[default]
    Leading,
    Digits(u64),
    /// Whitespace after the digits.
    Trailing(u64),
    /// Not a decimal number hyper takes.
    Invalid,
}

/// The last coding of a `Transfer-Encoding` value, matched against `chunked` as it arrives.
#[derive(Debug, Default)]
struct Coding {
    /// How much of `chunked` the coding matched so far; `None` once it differs.
    matched: Option<usize>,
    /// Whitespace followed the coding's first characters.
    trailing: bool,
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
                            | State::Head(_)
                            | State::Body(_)
                            | State::ChunkSize { .. }
                            | State::ChunkEnd
                            | State::Trailer(_) => State::Between,
                        };
                    }
                    continue;
                }
                // httparse skips the empty lines before a request line.
                State::Between if byte == b'\r' || byte == b'\n' => {}
                State::Between => {
                    self.started += 1;
                    self.state = State::Head(Head::new());
                    continue;
                }
                State::Head(head) => {
                    if let Some(next) = head.feed(byte) {
                        self.state = next;
                    }
                }
                State::ChunkSize { size, digits_ended } => {
                    if byte == b'\n' {
                        self.state = if *size == 0 {
                            State::Trailer(true)
                        } else {
                            State::ChunkData(*size)
                        };
                    } else if let (false, Some(digit)) = (*digits_ended, hex_digit(byte)) {
                        // An overflow is hyper's error, which ends the connection.
                        *size = size.saturating_mul(16).saturating_add(digit);
                    } else {
                        // Whitespace or an extension, up to the end of the line.
                        *digits_ended = true;
                    }
                }
                State::ChunkEnd => {
                    if byte == b'\n' {
                        self.state = State::ChunkSize {
                            size: 0,
                            digits_ended: false,
                        };
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

impl Head {
    fn new() -> Self {
        Self {
            request_line: true,
            line_empty: true,
            field: Field::Name(Vec::new()),
            content_length: 0,
            chunked: None,
        }
    }

    /// Follows one byte of the head; the next state once the head ends.
    fn feed(&mut self, byte: u8) -> Option<State> {
        if byte != b'\n' {
            if byte != b'\r' {
                self.line_empty = false;
            }
            if !self.request_line {
                self.field.feed(byte);
            }
            return None;
        }
        if self.request_line {
            self.request_line = false;
        } else if self.line_empty {
            return Some(match self.chunked {
                Some(true) => State::ChunkSize {
                    size: 0,
                    digits_ended: false,
                },
                None if self.content_length > 0 => State::Body(self.content_length),
                // A last coding other than chunked is hyper's error, which ends the connection.
                Some(false) | None => State::Between,
            });
        } else {
            self.end_line();
        }
        self.line_empty = true;
        self.field = Field::Name(Vec::new());
        None
    }

    /// Records what the header line just read says about the body.
    fn end_line(&mut self) {
        match std::mem::replace(&mut self.field, Field::Other) {
            Field::ContentLength(length) => {
                // An invalid value is hyper's error, which ends the connection.
                if let Some(value) = length.finish() {
                    self.content_length = value;
                }
            }
            Field::TransferEncoding(coding) => self.chunked = Some(coding.is_chunked()),
            Field::Name(_) | Field::Other => {}
        }
    }
}

impl Field {
    fn feed(&mut self, byte: u8) {
        match self {
            Self::Name(name) => {
                if byte == b':' {
                    *self = if name.as_slice() == CONTENT_LENGTH {
                        Self::ContentLength(Length::default())
                    } else if name.as_slice() == TRANSFER_ENCODING {
                        Self::TransferEncoding(Coding {
                            matched: Some(0),
                            trailing: false,
                        })
                    } else {
                        Self::Other
                    };
                } else if name.len() < TRANSFER_ENCODING.len() {
                    name.push(byte.to_ascii_lowercase());
                } else {
                    *self = Self::Other;
                }
            }
            Self::ContentLength(length) => length.feed(byte),
            Self::TransferEncoding(coding) => coding.feed(byte),
            Self::Other => {}
        }
    }
}

impl Length {
    fn feed(&mut self, byte: u8) {
        if byte == b',' {
            self.end_item();
            return;
        }
        let whitespace = is_whitespace(byte);
        self.item = match (std::mem::take(&mut self.item), byte) {
            (Item::Leading, _) if whitespace => Item::Leading,
            (Item::Leading, b'0'..=b'9') => Item::Digits(u64::from(byte - b'0')),
            (Item::Digits(value), b'0'..=b'9') => value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .map_or(Item::Invalid, Item::Digits),
            (Item::Digits(value) | Item::Trailing(value), _) if whitespace => Item::Trailing(value),
            (Item::Leading | Item::Digits(_) | Item::Trailing(_) | Item::Invalid, _) => {
                Item::Invalid
            }
        };
    }

    fn end_item(&mut self) {
        let item = match std::mem::take(&mut self.item) {
            Item::Digits(value) | Item::Trailing(value) => LengthValue::Valid(value),
            Item::Leading | Item::Invalid => LengthValue::Invalid,
        };
        self.value = match self.value {
            LengthValue::NoItem => item,
            previous if previous == item => previous,
            LengthValue::Valid(_) | LengthValue::Invalid => LengthValue::Invalid,
        };
    }

    /// The length, or `None` when hyper would refuse the value.
    fn finish(mut self) -> Option<u64> {
        self.end_item();
        match self.value {
            LengthValue::Valid(value) => Some(value),
            LengthValue::NoItem | LengthValue::Invalid => None,
        }
    }
}

impl Coding {
    fn feed(&mut self, byte: u8) {
        if byte == b',' {
            *self = Self {
                matched: Some(0),
                trailing: false,
            };
        } else if is_whitespace(byte) {
            self.trailing = self.matched != Some(0);
        } else {
            self.matched = match self.matched {
                Some(count)
                    if !self.trailing && CHUNKED.get(count) == Some(&byte.to_ascii_lowercase()) =>
                {
                    Some(count + 1)
                }
                Some(_) | None => None,
            };
        }
    }

    fn is_chunked(&self) -> bool {
        self.matched == Some(CHUNKED.len())
    }
}

/// The whitespace `str::trim` drops around a header value item; a carriage return is the end of
/// the line.
fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r')
}

fn hex_digit(byte: u8) -> Option<u64> {
    char::from(byte).to_digit(16).map(u64::from)
}

#[cfg(test)]
mod tests;
