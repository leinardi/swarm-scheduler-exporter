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

//! The `-run` flag of Go's test runner, for the integration runner. libtest's filter is a
//! substring, so `RUN='^TestNode_'` would select nothing and still pass; this keeps Go's semantics
//! instead.
//!
//! The pattern is split, as Go's testing package splits it, on every `/` and `|` outside a
//! bracket expression and outside parentheses (a backslash escapes the next byte): `/` separates
//! the levels of a test path (top-level test, subtest), and a top-level `|` separates whole
//! alternatives, so `^TestA$/x|^TestB$` is "subtest x of `TestA`, or `TestB`". Each element is an
//! unanchored regular expression, matched as Go's `regexp.MatchString` matches. A test path
//! deeper than the pattern matches on its first levels; one shallower matches if its levels do,
//! so a top-level test runs whenever its own level matches and its subtests are filtered in turn.
//!
//! Unlike Go, which warns "no tests to run" and passes, a non-empty pattern that selects no test
//! is an error: a typo in `RUN` must not certify coverage.

use std::fmt;

use regex::Regex;

/// A `-run` pattern that is not a valid regular expression, in Go's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPattern(String);

impl fmt::Display for InvalidPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidPattern {}

/// A non-empty pattern that selects no test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NothingSelected {
    /// The pattern, as given.
    pub pattern: String,
}

impl fmt::Display for NothingSelected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "--run {:?} selects no test", self.pattern)
    }
}

impl std::error::Error for NothingSelected {}

/// A parsed `-run` pattern.
#[derive(Debug, Clone)]
pub struct RunFilter {
    pattern: String,
    /// The alternatives, each a list of per-level expressions. An empty pattern is one
    /// alternative with no level, which matches everything.
    alternatives: Vec<Vec<Regex>>,
}

impl RunFilter {
    /// Parses `pattern`; an empty one selects every test.
    ///
    /// # Errors
    ///
    /// An element that does not compile, named as Go names it.
    pub fn parse(pattern: &str) -> Result<Self, InvalidPattern> {
        if pattern.is_empty() {
            return Ok(Self {
                pattern: String::new(),
                alternatives: vec![Vec::new()],
            });
        }
        let alternatives = split(pattern)
            .into_iter()
            .map(|levels| {
                levels
                    .into_iter()
                    .enumerate()
                    .map(|(index, element)| {
                        Regex::new(element).map_err(|error| {
                            InvalidPattern(format!(
                                "invalid regexp for element {index} of {pattern} ({element:?}): \
                                 {error}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            pattern: pattern.to_owned(),
            alternatives,
        })
    }

    /// The pattern, as given.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Whether the test at `path` (the top-level name, then each subtest name) runs.
    #[must_use]
    pub fn matches(&self, path: &[&str]) -> bool {
        self.alternatives.iter().any(|levels| {
            path.iter()
                .zip(levels)
                .all(|(name, level)| level.is_match(name))
        })
    }

    /// The top-level tests among `names` that run, in order.
    ///
    /// # Errors
    ///
    /// A non-empty pattern that selects none of them.
    pub fn select<'name>(&self, names: &[&'name str]) -> Result<Vec<&'name str>, NothingSelected> {
        let selected: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| self.matches(&[name]))
            .collect();
        if selected.is_empty() && !self.pattern.is_empty() {
            return Err(NothingSelected {
                pattern: self.pattern.clone(),
            });
        }
        Ok(selected)
    }
}

/// Go's `splitRegexp`: the alternatives, each split into its levels.
fn split(pattern: &str) -> Vec<Vec<&str>> {
    let bytes = pattern.as_bytes();
    let mut alternatives = Vec::new();
    let mut levels = Vec::new();
    let mut start = 0;
    let mut brackets = 0_usize;
    let mut parens = 0_isize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'[' => brackets += 1,
            // An unmatched ] is legal.
            b']' => brackets = brackets.saturating_sub(1),
            b'(' if brackets == 0 => parens += 1,
            b')' if brackets == 0 => parens -= 1,
            b'\\' => index += 1,
            b'/' if brackets == 0 && parens == 0 => {
                levels.push(&pattern[start..index]);
                start = index + 1;
            }
            b'|' if brackets == 0 && parens == 0 => {
                levels.push(&pattern[start..index]);
                start = index + 1;
                alternatives.push(std::mem::take(&mut levels));
            }
            _ => {}
        }
        index += 1;
    }
    levels.push(&pattern[start..]);
    alternatives.push(levels);
    alternatives
}

#[cfg(test)]
mod tests {
    use super::RunFilter;

    /// The Go integration suite's top-level tests.
    const SUITE: &[&str] = &[
        "TestCluster_Smoke",
        "TestContainers_OptIn",
        "TestEvents_ReconnectResyncs",
        "TestExporter_HealthcheckHealthy",
        "TestExporter_HealthcheckUnhealthy",
        "TestExporter_PortRetry",
        "TestGlobal_Constraints",
        "TestJobs_Finished",
        "TestNode_Down",
        "TestNode_Drain",
        "TestReplicated_Scale",
        "TestSeed_FailedThenRecovers",
        "TestService_Removed",
        "TestSnapshot_SteadyState",
        "TestSocketProxy_ExampleAllowlist",
        "TestUpdate_Completed",
        "TestUpdate_StartFirstFailure",
    ];

    fn select(pattern: &str) -> Vec<&'static str> {
        RunFilter::parse(pattern)
            .unwrap_or_else(|error| panic!("{pattern}: {error}"))
            .select(SUITE)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn a_prefix_selects_exactly_its_tests() {
        assert_eq!(select("^TestNode_"), ["TestNode_Down", "TestNode_Drain"]);
    }

    #[test]
    fn an_alternation_inside_parentheses_is_one_level() {
        assert_eq!(
            select("^Test(Cluster|Exporter)_"),
            [
                "TestCluster_Smoke",
                "TestExporter_HealthcheckHealthy",
                "TestExporter_HealthcheckUnhealthy",
                "TestExporter_PortRetry",
            ]
        );
    }

    #[test]
    fn a_top_level_alternation_separates_whole_paths() {
        let filter =
            RunFilter::parse("^TestNode_Down$/^none$|^TestJobs_").unwrap_or_else(|error| {
                panic!("{error}");
            });
        assert_eq!(
            filter
                .select(SUITE)
                .unwrap_or_else(|error| panic!("{error}")),
            ["TestJobs_Finished", "TestNode_Down"]
        );
        assert!(!filter.matches(&["TestNode_Down", "other"]));
        assert!(filter.matches(&["TestJobs_Finished", "other"]));
    }

    #[test]
    fn a_subtest_level_filters_the_subtests() {
        let filter = RunFilter::parse("^TestSocketProxy_ExampleAllowlist$/^denied$")
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            filter
                .select(SUITE)
                .unwrap_or_else(|error| panic!("{error}")),
            ["TestSocketProxy_ExampleAllowlist"]
        );
        assert!(filter.matches(&["TestSocketProxy_ExampleAllowlist", "denied"]));
        assert!(!filter.matches(&["TestSocketProxy_ExampleAllowlist", "scope"]));
        assert!(!filter.matches(&["TestSocketProxy_ExampleAllowlist", "denied_more"]));
        // A level the pattern does not have matches everything.
        assert!(
            RunFilter::parse("^TestSocketProxy_")
                .unwrap_or_else(|error| panic!("{error}"))
                .matches(&["TestSocketProxy_ExampleAllowlist", "scope"])
        );
    }

    #[test]
    fn elements_are_unanchored() {
        assert_eq!(select("Drain"), ["TestNode_Drain"]);
        assert_eq!(
            select("Update"),
            ["TestUpdate_Completed", "TestUpdate_StartFirstFailure"]
        );
        let filter = RunFilter::parse("Proxy/en").unwrap_or_else(|error| panic!("{error}"));
        assert!(filter.matches(&["TestSocketProxy_ExampleAllowlist", "denied"]));
        assert!(!filter.matches(&["TestSocketProxy_ExampleAllowlist", "scope"]));
    }

    #[test]
    fn a_slash_inside_brackets_does_not_split() {
        let filter = RunFilter::parse("^Test[/_]Node$").unwrap_or_else(|error| panic!("{error}"));
        assert!(filter.matches(&["Test/Node"]));
        assert!(filter.matches(&["Test_Node", "anything"]));
        // Negative control: outside brackets the same slash starts a subtest level.
        let split = RunFilter::parse("^Test$/^Node$").unwrap_or_else(|error| panic!("{error}"));
        assert!(!split.matches(&["Test/Node"]));
        assert!(split.matches(&["Test", "Node"]));
        // And an escaped slash is not a separator either.
        let escaped = RunFilter::parse(r"^a\/b$").unwrap_or_else(|error| panic!("{error}"));
        assert!(escaped.matches(&["a/b"]));
    }

    #[test]
    fn a_pattern_matching_nothing_fails() {
        let error = RunFilter::parse("^TestNode_Typo")
            .unwrap_or_else(|error| panic!("{error}"))
            .select(SUITE)
            .err()
            .unwrap_or_else(|| panic!("a pattern matching nothing selected tests"));
        assert_eq!(
            error.to_string(),
            r#"--run "^TestNode_Typo" selects no test"#
        );
    }

    #[test]
    fn an_empty_pattern_runs_everything() {
        assert_eq!(select(""), SUITE);
        let filter = RunFilter::parse("").unwrap_or_else(|error| panic!("{error}"));
        assert!(filter.matches(&["TestSocketProxy_ExampleAllowlist", "scope"]));
        // With an empty suite nothing is selected, and that is not the filter's error.
        assert_eq!(filter.select(&[]), Ok(Vec::new()));
    }

    #[test]
    fn an_invalid_element_is_named() {
        let error = RunFilter::parse("^TestNode_/(")
            .err()
            .unwrap_or_else(|| panic!("an unbalanced parenthesis compiled"));
        assert!(
            error
                .to_string()
                .starts_with(r#"invalid regexp for element 1 of ^TestNode_/( ("("): "#),
            "{error}"
        );
    }
}
