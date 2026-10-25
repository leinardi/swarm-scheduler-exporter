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

//! Go's `strconv.Quote`, and the `IsPrint` and `IsSpace` predicates behind it and slog's quoting.
//!
//! The predicates read Go's Unicode 15 tables, generated from Go: Rust's standard library follows
//! a later Unicode version (17 in Rust 1.98), so its notion of printable differs for the code
//! points assigned since (`strconv.Quote` writes `\u0897` where `escape_debug` writes `\u{897}`,
//! and U+1CC00 is escaped by Go and printed raw by Rust). Input is a `&str`, so the invalid-byte
//! branch of `strconv.Quote` has no counterpart.

use std::cmp::Ordering;

use super::unicode_tables::{PRINT, SPACE};

const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// Go's `strconv.IsPrint`, which equals `unicode.IsPrint`: letters, marks, numbers, punctuation,
/// symbols and the ASCII space, in Go's Unicode 15 tables.
#[must_use]
pub fn is_print(character: char) -> bool {
    in_table(PRINT, character)
}

/// Go's `unicode.IsSpace`, in Go's Unicode 15 tables.
#[must_use]
pub fn is_space(character: char) -> bool {
    in_table(SPACE, character)
}

fn in_table(table: &[(u32, u32)], character: char) -> bool {
    let code = u32::from(character);
    table
        .binary_search_by(|&(low, high)| {
            if high < code {
                Ordering::Less
            } else if low > code {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// `strconv.Quote(text)`.
#[must_use]
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    push_quoted(&mut out, text);
    out
}

/// Appends `strconv.Quote(text)` to `out`.
pub fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        if character == '"' || character == '\\' {
            out.push('\\');
            out.push(character);
            continue;
        }
        if is_print(character) {
            out.push(character);
            continue;
        }
        match character {
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0b' => out.push_str("\\v"),
            _ if character < ' ' || character == '\x7f' => {
                push_hex_escape(out, 'x', u32::from(character), 2);
            }
            _ if u32::from(character) < 0x1_0000 => {
                push_hex_escape(out, 'u', u32::from(character), 4);
            }
            _ => push_hex_escape(out, 'U', u32::from(character), 8),
        }
    }
    out.push('"');
}

/// Appends a backslash, `kind`, and `value` as `digits` lowercase hex digits.
pub fn push_hex_escape(out: &mut String, kind: char, value: u32, digits: u32) {
    out.push('\\');
    out.push(kind);
    for index in (0..digits).rev() {
        let nibble = (value >> (index * 4)) & 0xF;
        out.push(char::from(LOWER_HEX[nibble as usize]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's `strconv.Quote` on the cases that tell Unicode versions and escape forms apart.
    #[test]
    fn quote_matches_go() {
        let cases = [
            ("", r#""""#),
            ("plain", r#""plain""#),
            ("say \"hi\"", r#""say \"hi\"""#),
            (r"C:\path", r#""C:\\path""#),
            ("\x07\x08\x0c\n\r\t\x0b", r#""\a\b\f\n\r\t\v""#),
            ("a\x01b\x7f", r#""a\x01b\x7f""#),
            ("nbsp\u{a0}", r#""nbsp\u00a0""#),
            ("ls\u{2028}", r#""ls\u2028""#),
            ("repl\u{fffd}", "\"repl\u{fffd}\""),
            ("ok 🚀", "\"ok 🚀\""),
            // Assigned after Unicode 15: Go escapes them, Rust 1.98 calls them printable.
            ("\u{897}", r#""\u0897""#),
            ("\u{1cc00}", r#""\U0001cc00""#),
            ("\u{e0001}", r#""\U000e0001""#),
        ];
        for (input, want) in cases {
            assert_eq!(quote(input), want, "quote({input:?})");
        }
    }

    #[test]
    fn is_space_follows_go() {
        for character in [
            ' ', '\t', '\n', '\x0b', '\x0c', '\r', '\u{85}', '\u{a0}', '\u{2028}',
        ] {
            assert!(is_space(character), "{character:?}");
        }
        for character in ['a', '\u{200b}', '\u{180e}'] {
            assert!(!is_space(character), "{character:?}");
        }
    }
}
