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

//! slog's `TextHandler` and `JSONHandler`: a port of their shared `commonHandler`, step for
//! step, so the output matches byte for byte, quirks included.
//!
//! - `with_attrs` formats its attributes once, under the groups open at that moment, and
//!   `with_group` opens a group lazily: a JSON group appears only once something is written in
//!   it.
//! - When the time is not logged, the exporter installs a `ReplaceAttr` that drops every
//!   attribute keyed `time`, at any depth, the record's own time included (Go's `timeStripper`).
//!   A group whose attributes are all dropped is erased, but, as in slog, the separator and the
//!   text key prefix it set are not restored.
//! - Strings in JSON go through slog's escaper (`\u0008`, `<` literal); a `[]string` goes through
//!   encoding/json's (`\b`). Floats are `'g'` in text and encoding/json's layout in JSON, where
//!   NaN and the infinities become the string `!ERROR:json: unsupported value: NaN`.

use crate::go_compat::{duration, float, quote, time_layout};

use super::Level;
use super::value::{Attr, Time, Value};

/// The time key the time stripper drops.
const TIME_KEY: &str = "time";

/// A text or JSON handler: the options and what `with_attrs` and `with_group` accumulated.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SlogHandler {
    json: bool,
    /// Whether the exporter's time stripper is installed as `ReplaceAttr`.
    strip_time: bool,
    /// The attributes `with_attrs` formatted, in output form.
    preformatted: String,
    /// The text key prefix of the groups opened in `preformatted`.
    group_prefix: String,
    /// Every group from `with_group`, opened or not.
    groups: Vec<String>,
    /// How many of `groups` are open in `preformatted`.
    open_groups: usize,
}

impl SlogHandler {
    pub(super) fn new(json: bool, include_time: bool) -> Self {
        Self {
            json,
            strip_time: !include_time,
            preformatted: String::new(),
            group_prefix: String::new(),
            groups: Vec::new(),
            open_groups: 0,
        }
    }

    fn attr_separator(&self) -> &'static str {
        if self.json { "," } else { " " }
    }

    /// `commonHandler.withAttrs`.
    pub(super) fn with_attrs(&self, attrs: &[Attr]) -> Self {
        // Empty groups are ignored, so a list of nothing else changes nothing.
        if attrs.iter().all(Attr::is_empty_group) {
            return self.clone();
        }
        let mut next = self.clone();
        let mut buffer = std::mem::take(&mut next.preformatted);
        let mut state = HandleState::new(&next, &mut buffer);
        state.prefix.push_str(&self.group_prefix);
        if !self.preformatted.is_empty() {
            state.separator = self.attr_separator();
            if self.json && self.preformatted.ends_with('{') {
                state.separator = "";
            }
        }
        // Remember the position, in case every attribute turns out empty.
        let position = state.buffer.len();
        state.open_groups();
        let appended = state.append_attrs(attrs);
        let prefix = std::mem::take(&mut state.prefix);
        if appended {
            next.group_prefix = prefix;
            next.open_groups = next.groups.len();
        } else {
            buffer.truncate(position);
        }
        next.preformatted = buffer;
        next
    }

    /// `commonHandler.withGroup`.
    pub(super) fn with_group(&self, name: &str) -> Self {
        let mut next = self.clone();
        next.groups.push(name.to_owned());
        next
    }

    /// `commonHandler.handle`: one record, newline included. `time` is `None` for Go's zero
    /// time, which slog leaves out.
    pub(super) fn format(
        &self,
        time: Option<Time>,
        level: Level,
        message: &str,
        attrs: &[Attr],
    ) -> String {
        let mut buffer = String::with_capacity(128);
        let mut state = HandleState::new(self, &mut buffer);
        if self.json {
            state.buffer.push('{');
        }
        if let Some(time) = time {
            if self.strip_time {
                state.append_attr(&Attr::new(TIME_KEY, Value::Time(time)));
            } else {
                state.append_key(TIME_KEY);
                state.append_time(time);
            }
        }
        state.append_key("level");
        state.append_string(&level.to_string());
        state.append_key("msg");
        state.append_string(message);
        state.append_non_built_ins(attrs);
        buffer.push('\n');
        buffer
    }
}

/// slog's `handleState`: the buffer, the separator owed before the next key, and the text key
/// prefix of the open groups.
struct HandleState<'handler, 'buffer> {
    handler: &'handler SlogHandler,
    buffer: &'buffer mut String,
    separator: &'static str,
    prefix: String,
}

impl<'handler, 'buffer> HandleState<'handler, 'buffer> {
    fn new(handler: &'handler SlogHandler, buffer: &'buffer mut String) -> Self {
        Self {
            handler,
            buffer,
            separator: "",
            prefix: String::new(),
        }
    }

    fn append_non_built_ins(&mut self, attrs: &[Attr]) {
        let handler = self.handler;
        if !handler.preformatted.is_empty() {
            self.buffer.push_str(self.separator);
            self.buffer.push_str(&handler.preformatted);
            self.separator = handler.attr_separator();
            if handler.json && handler.preformatted.ends_with('{') {
                self.separator = "";
            }
        }
        // Record attributes are in the groups from with_group; without any, no group is output.
        let mut open_groups = handler.open_groups;
        if !attrs.is_empty() {
            self.prefix.push_str(&handler.group_prefix);
            let position = self.buffer.len();
            self.open_groups();
            open_groups = handler.groups.len();
            let mut empty = true;
            for attr in attrs {
                if self.append_attr(attr) {
                    empty = false;
                }
            }
            if empty {
                self.buffer.truncate(position);
                open_groups = handler.open_groups;
            }
        }
        if handler.json {
            for _ in 0..open_groups {
                self.buffer.push('}');
            }
            self.buffer.push('}');
        }
    }

    fn open_groups(&mut self) {
        let handler = self.handler;
        for name in &handler.groups[handler.open_groups..] {
            self.open_group(name);
        }
    }

    fn open_group(&mut self, name: &str) {
        if self.handler.json {
            self.append_key(name);
            self.buffer.push('{');
            self.separator = "";
        } else {
            self.prefix.push_str(name);
            self.prefix.push('.');
        }
    }

    fn close_group(&mut self, name: &str) {
        if self.handler.json {
            self.buffer.push('}');
        } else {
            let length = self.prefix.len().saturating_sub(name.len() + 1);
            self.prefix.truncate(length);
        }
        self.separator = self.handler.attr_separator();
    }

    /// Reports whether anything was appended.
    fn append_attrs(&mut self, attrs: &[Attr]) -> bool {
        let mut appended = false;
        for attr in attrs {
            if self.append_attr(attr) {
                appended = true;
            }
        }
        appended
    }

    /// slog's `appendAttr`; reports whether anything was appended.
    fn append_attr(&mut self, attr: &Attr) -> bool {
        let is_group = matches!(attr.value, Value::Group(_));
        // The time stripper replaces every non-group attribute keyed "time" with the empty Attr.
        if self.handler.strip_time && !is_group && attr.key == TIME_KEY {
            return false;
        }
        if attr.is_empty() {
            return false;
        }
        if let Value::Group(children) = &attr.value {
            // Only non-empty groups are output; one whose attributes are all empty is erased.
            if !children.is_empty() {
                let position = self.buffer.len();
                if !attr.key.is_empty() {
                    self.open_group(&attr.key);
                }
                if !self.append_attrs(children) {
                    self.buffer.truncate(position);
                    return false;
                }
                if !attr.key.is_empty() {
                    self.close_group(&attr.key);
                }
            }
        } else {
            self.append_key(&attr.key);
            self.append_value(&attr.value);
        }
        true
    }

    fn append_key(&mut self, key: &str) {
        self.buffer.push_str(self.separator);
        if self.prefix.is_empty() {
            self.append_string(key);
        } else {
            let prefix = std::mem::take(&mut self.prefix);
            self.append_two_strings(&prefix, key);
            self.prefix = prefix;
        }
        self.buffer.push(if self.handler.json { ':' } else { '=' });
        self.separator = self.handler.attr_separator();
    }

    /// `appendString(first + second)`.
    fn append_two_strings(&mut self, first: &str, second: &str) {
        if self.handler.json {
            self.buffer.push('"');
            push_slog_json_escaped(self.buffer, first);
            push_slog_json_escaped(self.buffer, second);
            self.buffer.push('"');
        } else if !needs_quoting(first) && !needs_quoting(second) {
            self.buffer.push_str(first);
            self.buffer.push_str(second);
        } else {
            quote::push_quoted(self.buffer, &format!("{first}{second}"));
        }
    }

    fn append_string(&mut self, text: &str) {
        if self.handler.json {
            self.buffer.push('"');
            push_slog_json_escaped(self.buffer, text);
            self.buffer.push('"');
        } else if needs_quoting(text) {
            quote::push_quoted(self.buffer, text);
        } else {
            self.buffer.push_str(text);
        }
    }

    fn append_error(&mut self, error: &str) {
        self.append_string(&format!("!ERROR:{error}"));
    }

    fn append_time(&mut self, time: Time) {
        if self.handler.json {
            let year = time.offset.to_datetime(time.timestamp).year();
            if !(0..10_000).contains(&year) {
                self.append_error("time.Time year outside of range [0,9999]");
            }
            self.buffer.push('"');
            time_layout::push_rfc3339_nano(self.buffer, time.timestamp, time.offset);
            self.buffer.push('"');
        } else {
            time_layout::push_rfc3339_millis(self.buffer, time.timestamp, time.offset);
        }
    }

    fn append_value(&mut self, value: &Value) {
        if self.handler.json {
            self.append_json_value(value);
        } else {
            self.append_text_value(value);
        }
    }

    /// `appendTextValue`.
    fn append_text_value(&mut self, value: &Value) {
        match value {
            Value::Str(text) | Value::Error(text) => self.append_string(text),
            Value::Time(time) => self.append_time(*time),
            // %+v of a []string, and of nil.
            Value::Strings(items) => self.append_string(&format!("[{}]", items.join(" "))),
            Value::Nil => self.append_string("<nil>"),
            Value::Int(number) => self.buffer.push_str(&number.to_string()),
            Value::Uint(number) => self.buffer.push_str(&number.to_string()),
            Value::Float(number) => float::push_g(self.buffer, *number),
            Value::Bool(flag) => self.buffer.push_str(if *flag { "true" } else { "false" }),
            Value::Duration(nanos) => duration::push(self.buffer, *nanos),
            // Groups never reach a value: append_attr walks them.
            Value::Group(_) => {}
        }
    }

    /// `appendJSONValue`.
    fn append_json_value(&mut self, value: &Value) {
        match value {
            Value::Str(text) | Value::Error(text) => self.append_string(text),
            Value::Int(number) | Value::Duration(number) => {
                self.buffer.push_str(&number.to_string());
            }
            Value::Uint(number) => self.buffer.push_str(&number.to_string()),
            Value::Float(number) => match float::format_json(*number) {
                Ok(text) => self.buffer.push_str(&text),
                Err(error) => self.append_error(&error.to_string()),
            },
            Value::Bool(flag) => self.buffer.push_str(if *flag { "true" } else { "false" }),
            Value::Time(time) => self.append_time(*time),
            // json.Marshal of a []string, through an encoder that does not escape HTML.
            Value::Strings(items) => {
                self.buffer.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.buffer.push(',');
                    }
                    self.buffer.push('"');
                    push_encoding_json_escaped(self.buffer, item);
                    self.buffer.push('"');
                }
                self.buffer.push(']');
            }
            Value::Nil => self.buffer.push_str("null"),
            Value::Group(_) => {}
        }
    }
}

/// encoding/json's `safeSet`: the ASCII bytes a JSON string holds unescaped, every one but the
/// control characters, `"` and `\`.
fn json_safe(byte: u8) -> bool {
    byte >= 0x20 && byte != b'"' && byte != b'\\'
}

/// slog's `needsQuoting`, for the text handler.
fn needs_quoting(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    text.chars().any(|character| {
        if character.is_ascii() {
            // Quote anything except a backslash that would need quoting in a JSON string, as
            // well as space and '='.
            let byte = character as u8;
            byte != b'\\' && (byte == b' ' || byte == b'=' || !json_safe(byte))
        } else {
            character == char::REPLACEMENT_CHARACTER
                || quote::is_space(character)
                || !quote::is_print(character)
        }
    })
}

/// slog's `appendEscapedJSONString`: control characters other than `\n`, `\r` and `\t` as
/// `\u00XX`, U+2028 and U+2029 escaped, `<`, `>` and `&` literal.
fn push_slog_json_escaped(out: &mut String, text: &str) {
    push_json_escaped(out, text, false);
}

/// encoding/json's string encoder without HTML escaping: as slog's, but with `\b` and `\f`.
fn push_encoding_json_escaped(out: &mut String, text: &str) {
    push_json_escaped(out, text, true);
}

fn push_json_escaped(out: &mut String, text: &str, short_backspace_and_form_feed: bool) {
    for character in text.chars() {
        if character.is_ascii() {
            let byte = character as u8;
            if json_safe(byte) {
                out.push(character);
                continue;
            }
            match byte {
                b'\\' | b'"' => {
                    out.push('\\');
                    out.push(character);
                }
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                0x08 if short_backspace_and_form_feed => out.push_str("\\b"),
                0x0c if short_backspace_and_form_feed => out.push_str("\\f"),
                _ => quote::push_hex_escape(out, 'u', u32::from(byte), 4),
            }
        } else if character == '\u{2028}' || character == '\u{2029}' {
            quote::push_hex_escape(out, 'u', u32::from(character), 4);
        } else {
            out.push(character);
        }
    }
}
