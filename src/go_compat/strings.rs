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

//! Ports of the `strings` functions whose Rust counterparts differ: `strings.EqualFold`
//! (Unicode simple case folding, where `eq_ignore_ascii_case` is ASCII only and `to_lowercase`
//! uses full mappings), `strings.ToLower` (per code point, so no final-sigma rule and no
//! two-character results) and `strings.TrimSpace`, all on Go's Unicode 15 tables.

use super::quote::is_space;
use super::unicode_case::{ALTERNATE, SIMPLE_FOLD, TO_LOWER};

/// Maps `character` through one of the generated tables.
fn lookup(table: &[(u32, u32, u32, i32)], character: char) -> char {
    let code = u32::from(character);
    let index = table.partition_point(|&(start, ..)| start <= code);
    let Some(&(start, count, step, delta)) = index.checked_sub(1).and_then(|last| table.get(last))
    else {
        return character;
    };
    let offset = code - start;
    if offset % step != 0 || offset / step >= count {
        return character;
    }
    let mapped = if delta == ALTERNATE {
        if offset % 2 == 0 { code + 1 } else { code - 1 }
    } else {
        code.wrapping_add_signed(delta)
    };
    char::from_u32(mapped).unwrap_or(character)
}

/// Go's `unicode.SimpleFold`: the next code point of `character`'s case-fold orbit, itself when
/// it has none.
#[must_use]
pub fn simple_fold(character: char) -> char {
    lookup(SIMPLE_FOLD, character)
}

/// Go's `unicode.ToLower`.
#[must_use]
pub fn to_lower_char(character: char) -> char {
    lookup(TO_LOWER, character)
}

/// Go's `strings.ToLower`.
#[must_use]
pub fn to_lower(text: &str) -> String {
    text.chars().map(to_lower_char).collect()
}

/// Go's `strings.EqualFold`: equal under Unicode simple case folding.
#[must_use]
pub fn equal_fold(left: &str, right: &str) -> bool {
    let mut right_chars = right.chars();
    for left_char in left.chars() {
        let Some(right_char) = right_chars.next() else {
            return false;
        };
        if left_char == right_char {
            continue;
        }
        let (low, high) = if left_char < right_char {
            (left_char, right_char)
        } else {
            (right_char, left_char)
        };
        if high.is_ascii() {
            if low.is_ascii_uppercase() && u32::from(high) == u32::from(low) + 32 {
                continue;
            }
            return false;
        }
        // The orbit of `low`, walked until it reaches or passes `high`.
        let mut folded = simple_fold(low);
        while folded != low && folded < high {
            folded = simple_fold(folded);
        }
        if folded != high {
            return false;
        }
    }
    right_chars.next().is_none()
}

/// Go's `strings.TrimSpace`: leading and trailing Unicode white space removed.
#[must_use]
pub fn trim_space(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Go's `strings.TrimSpace` over bytes that need not be UTF-8, as a Go string holds: leading and
/// trailing Unicode white space removed, stopping at the first byte that does not decode, which
/// Go reads as `utf8.RuneError` and keeps.
#[must_use]
pub fn trim_space_bytes(mut bytes: &[u8]) -> &[u8] {
    while let Some((character, width)) = first_char(bytes) {
        if !is_space(character) {
            break;
        }
        bytes = &bytes[width..];
    }
    while let Some((character, width)) = last_char(bytes) {
        if !is_space(character) {
            break;
        }
        bytes = &bytes[..bytes.len() - width];
    }
    bytes
}

/// The character `bytes` starts with, and its width; none when they do not start with one.
fn first_char(bytes: &[u8]) -> Option<(char, usize)> {
    (1..=bytes.len().min(4)).find_map(|width| {
        std::str::from_utf8(&bytes[..width])
            .ok()
            .and_then(|text| text.chars().next())
            .map(|character| (character, width))
    })
}

/// The character `bytes` ends with, and its width; none when they do not end with one.
fn last_char(bytes: &[u8]) -> Option<(char, usize)> {
    (1..=bytes.len().min(4)).find_map(|width| {
        std::str::from_utf8(&bytes[bytes.len() - width..])
            .ok()
            .and_then(|text| text.chars().next())
            .map(|character| (character, width))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Unicode 16 and 17 case pairs Rust knows and Go's Unicode 15 does not.
    /// Go's `TrimSpace` on bytes: Unicode spaces go, an undecodable byte stops the trim.
    #[test]
    fn trim_space_bytes_is_go_s() {
        assert_eq!(trim_space_bytes("\u{85} x \u{3000}".as_bytes()), b"x");
        assert_eq!(trim_space_bytes(b"\xff x \xff"), b"\xff x \xff");
        assert_eq!(trim_space_bytes(b" \t\r\n"), b"");
        assert_eq!(trim_space_bytes(b"  partial reason  "), b"partial reason");
    }

    #[test]
    fn folding_uses_unicode_15() {
        assert!(!equal_fold("\u{1C89}", "\u{1C8A}"));
        // Rust's tables know the pair.
        assert_eq!("\u{1C89}".to_lowercase(), "\u{1C8A}");
        assert_eq!(to_lower("\u{1C89}"), "\u{1C89}");
    }

    #[test]
    fn folding_walks_whole_orbits() {
        assert!(equal_fold("k", "\u{212A}"));
        assert!(equal_fold("K", "\u{212A}"));
        assert!(equal_fold("S", "\u{17F}"));
        assert!(equal_fold("\u{3C2}", "\u{3A3}"));
        assert!(!equal_fold("ss", "\u{DF}"));
        assert!(!equal_fold("a", "ab"));
        assert!(!equal_fold("ab", "a"));
        assert_eq!(to_lower("ΑΣ"), "ασ");
        assert_eq!(to_lower("İ"), "i");
    }
}
