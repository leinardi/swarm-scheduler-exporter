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

//! The exporter's plain format:
//!
//! ```text
//! level=INFO hello world k=v foo=bar
//! ```
//!
//! with `time=<RFC3339Nano>` first when the time is logged. One record is always one line, and
//! nothing in a message, key or value can forge a field: the message is escaped, and a key or
//! string value that is not plainly safe is quoted (`strconv.Quote`).
//!
//! Its quirks are kept: a level that is not exactly info, warn or error reads `ERROR` above
//! debug; attributes added by `with_attrs` are qualified with every group, those opened after
//! them included; a group without a key is dropped; and a group from `with_attrs` with no
//! attributes is written `key={}`.

use crate::go_compat::{duration, float, quote, time_layout};

use super::Level;
use super::value::{Attr, Time, Value};

/// The plain handler: the attributes from `with_attrs` and the groups from `with_group`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct PlainHandler {
    prefix_attrs: Vec<Attr>,
    groups: Vec<String>,
}

impl PlainHandler {
    /// `WithAttrs`, copy on write; no attributes change nothing.
    pub(super) fn with_attrs(&self, attrs: &[Attr]) -> Self {
        let mut next = self.clone();
        next.prefix_attrs.extend_from_slice(attrs);
        next
    }

    /// `WithGroup`: the name is trimmed, and a blank one changes nothing.
    pub(super) fn with_group(&self, name: &str) -> Self {
        let name = name.trim();
        let mut next = self.clone();
        if !name.is_empty() {
            next.groups.push(name.to_owned());
        }
        next
    }

    /// `Handle`: one record, newline included. `time` is `None` when the time is not logged, or
    /// is Go's zero time.
    pub(super) fn format(
        &self,
        time: Option<Time>,
        level: Level,
        message: &str,
        attrs: &[Attr],
    ) -> String {
        let mut buffer = String::with_capacity(128);
        if let Some(time) = time {
            buffer.push_str("time=");
            time_layout::push_rfc3339_nano(&mut buffer, time.timestamp, time.offset);
            buffer.push(' ');
        }
        buffer.push_str("level=");
        buffer.push_str(level_to_upper(level));
        if !message.is_empty() {
            buffer.push(' ');
            push_escaped_unquoted(&mut buffer, message);
        }
        for attr in self.prefix_attrs.iter().chain(attrs) {
            write_attr_kv(&mut buffer, &qualify(&self.groups, attr));
        }
        buffer.push('\n');
        buffer
    }
}

/// `levelToUpper`.
pub(super) fn level_to_upper(level: Level) -> &'static str {
    if level <= Level::DEBUG {
        "DEBUG"
    } else if level == Level::INFO {
        "INFO"
    } else if level == Level::WARN {
        "WARN"
    } else {
        "ERROR"
    }
}

/// `qualify`: the key under the nested group prefixes (`g1.g2.key`).
pub(super) fn qualify(groups: &[String], attr: &Attr) -> Attr {
    if groups.is_empty() {
        return attr.clone();
    }
    let mut key = groups.join(".");
    if !attr.key.is_empty() {
        key.push('.');
        key.push_str(&attr.key);
    }
    Attr {
        key,
        value: attr.value.clone(),
    }
}

/// `skipAttr`: the empty Attr, and a group without a key, are not written at all.
fn skip_attr(attr: &Attr) -> bool {
    attr.is_empty() || (attr.key.is_empty() && matches!(attr.value, Value::Group(_)))
}

/// `writeAttrKV`: ` key=value`.
fn write_attr_kv(buffer: &mut String, attr: &Attr) {
    if skip_attr(attr) {
        return;
    }
    buffer.push(' ');
    write_kv(buffer, &attr.key, &attr.value);
}

/// `writeKV` without the leading space.
fn write_kv(buffer: &mut String, key: &str, value: &Value) {
    if is_raw_key(key) {
        buffer.push_str(key);
    } else {
        quote::push_quoted(buffer, key);
    }
    buffer.push('=');
    match value {
        Value::Group(children) => write_group_braced(buffer, key, children),
        Value::Str(text) | Value::Error(text) => write_text_value(buffer, text),
        Value::Int(number) => buffer.push_str(&number.to_string()),
        Value::Uint(number) => buffer.push_str(&number.to_string()),
        Value::Float(number) => float::push_g(buffer, *number),
        Value::Bool(flag) => buffer.push_str(if *flag { "true" } else { "false" }),
        Value::Time(time) => {
            if time_layout::is_go_zero_time(time.timestamp) {
                buffer.push('0');
            } else {
                time_layout::push_rfc3339_nano(buffer, time.timestamp, time.offset);
            }
        }
        Value::Duration(nanos) => duration::push(buffer, *nanos),
        // fmt.Sprint of a []string, and of nil.
        Value::Strings(items) => write_text_value(buffer, &format!("[{}]", items.join(" "))),
        Value::Nil => write_text_value(buffer, "<nil>"),
    }
}

/// `writeGroupBraced`: `key={key.a=1 key.b="two"}`, the children's keys under the parent's.
fn write_group_braced(buffer: &mut String, parent_key: &str, children: &[Attr]) {
    buffer.push('{');
    for (index, child) in children.iter().enumerate() {
        if index > 0 {
            buffer.push(' ');
        }
        let mut key = parent_key.to_owned();
        if !child.key.is_empty() {
            key.push('.');
            key.push_str(&child.key);
        }
        let qualified = Attr {
            key,
            value: child.value.clone(),
        };
        if !skip_attr(&qualified) {
            write_kv(buffer, &qualified.key, &qualified.value);
        }
    }
    buffer.push('}');
}

/// `isRawKey`: non-empty and made of `[A-Za-z0-9_.-]`.
fn is_raw_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// `writeTextValue`: raw when unambiguous, quoted otherwise.
fn write_text_value(buffer: &mut String, text: &str) {
    if is_raw_value(text) {
        buffer.push_str(text);
    } else {
        quote::push_quoted(buffer, text);
    }
}

/// `isRawValue`: non-empty, every character printable, and none of the delimiters (space, quote,
/// equals, backslash, braces).
fn is_raw_value(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| quote::is_print(character) && !" \"=\\{}".contains(character))
}

/// `escapeUnquoted`, for the message: a backslash is doubled, a printable character is written
/// as is, `\n`, `\r` and `\t` are escaped by name, and any other character as `\xNN` below 0x80,
/// else `\uNNNN` or `\UNNNNNNNN`.
fn push_escaped_unquoted(buffer: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '\\' => buffer.push_str("\\\\"),
            _ if quote::is_print(character) => buffer.push(character),
            '\n' => buffer.push_str("\\n"),
            '\r' => buffer.push_str("\\r"),
            '\t' => buffer.push_str("\\t"),
            _ if character.is_ascii() => {
                quote::push_hex_escape(buffer, 'x', u32::from(character), 2);
            }
            _ if u32::from(character) <= 0xFFFF => {
                quote::push_hex_escape(buffer, 'u', u32::from(character), 4);
            }
            _ => quote::push_hex_escape(buffer, 'U', u32::from(character), 8),
        }
    }
}
