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

//! The build's version stamps, logged at startup and exported as `swarm_exporter_build_info`.
//! They are read at compile time from `SWARM_SCHEDULER_EXPORTER_VERSION`, `_COMMIT` and `_DATE`
//! (the make targets set them from git and the clock, as Go's `-ldflags -X` did), and fall back to
//! Go's defaults when unset.

/// The version, `dev` unless stamped.
pub const VERSION: &str = stamp(option_env!("SWARM_SCHEDULER_EXPORTER_VERSION"), "dev");

/// The commit, `none` unless stamped.
pub const COMMIT: &str = stamp(option_env!("SWARM_SCHEDULER_EXPORTER_COMMIT"), "none");

/// The build date, `unknown` unless stamped.
pub const DATE: &str = stamp(option_env!("SWARM_SCHEDULER_EXPORTER_DATE"), "unknown");

/// The stamp, or `default` when the variable was unset at build time. A variable set to the empty
/// string stamps the empty string, as `-X main.version=` did.
const fn stamp(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(value) => value,
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_stamps_fall_back_to_go_defaults() {
        assert_eq!(stamp(None, "dev"), "dev");
        assert_eq!(stamp(Some(""), "dev"), "");
        assert_eq!(stamp(Some("v1.2.3"), "dev"), "v1.2.3");
    }
}
