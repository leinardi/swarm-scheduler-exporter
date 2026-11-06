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

//! The case and space rules of the placement constraints and the container labels, in std.
//!
//! The Go exporter used `strings.EqualFold`, `ToLower` and `TrimSpace`. The values compared are
//! Docker's own (keys such as `node.role`, states, architectures, label values), ASCII in
//! practice, where these agree. They can differ for a few non-ASCII letters: Go folds with simple
//! case folding (`ſ` equals `s`; here it does not), and Rust's lowercase of `İ` is two characters.

/// Whether `left` and `right` are equal ignoring case.
pub(crate) fn equal_fold(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right) || left.to_lowercase() == right.to_lowercase()
}

/// `text` in lowercase.
pub(crate) fn to_lower(text: &str) -> String {
    text.to_lowercase()
}

/// `text` without leading and trailing white space (Unicode `White_Space`, as Go's `TrimSpace`).
pub(crate) fn trim_space(text: &str) -> &str {
    text.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_unicode_letters_fold() {
        assert!(equal_fold("Node.ID", "node.id"));
        assert!(equal_fold("ÄÖ", "äö"));
        assert!(
            equal_fold("\u{212a}", "k"),
            "the Kelvin sign lowercases to k"
        );
        assert!(!equal_fold("a", "b"));
        assert!(!equal_fold("ſ", "s"), "Go's simple folding is not applied");
        assert_eq!(to_lower("X86_64"), "x86_64");
    }

    #[test]
    fn trimming_removes_unicode_white_space() {
        assert_eq!(trim_space(" \t a b\u{a0}\n"), "a b");
        assert_eq!(trim_space("\u{85}x"), "x");
        assert_eq!(trim_space(""), "");
    }
}
