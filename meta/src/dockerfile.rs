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

//! Reads a Dockerfile closely enough to assert the image invariants that docs/release.md and the
//! Dockerfile's own comments state: stages, their bases and the instructions inside them.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

/// One Dockerfile instruction with its continuation lines joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// The instruction, upper-cased: FROM, RUN, COPY, ...
    pub keyword: String,
    /// Everything after the keyword, with continuations joined by a space.
    pub args: String,
}

/// One FROM and the instructions up to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    /// The AS alias, empty for an unnamed stage.
    pub name: String,
    /// The --platform value, empty when the stage has none.
    pub platform: String,
    /// The base reference exactly as written.
    pub image: String,
    /// The instructions, the FROM itself excluded.
    pub instructions: Vec<Instruction>,
}

impl Stage {
    /// The stage's instructions, one `KEYWORD args` per line.
    #[must_use]
    pub fn joined(&self) -> String {
        self.instructions
            .iter()
            .map(|instruction| format!("{} {}", instruction.keyword, instruction.args))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A Dockerfile this reader cannot take apart; the assertions refuse it rather than guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed(String);

impl fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "malformed Dockerfile: {}", self.0)
    }
}

impl std::error::Error for Malformed {}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// A literal registry reference with a tag and a sha256 digest. A variable anywhere in the
/// reference makes it unpinnable by a reader: the value lives in a build argument.
static DIGEST_PINNED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+@sha256:[0-9a-f]{64}$"));

/// A literal registry reference with a tag and no digest: the policy for the dhi.io bases, which
/// dhi.io republishes with security fixes (docs/release.md).
static TAG_PINNED: LazyLock<Regex> = LazyLock::new(|| regex(r"^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+$"));

/// One `# key=value` line of the block `BuildKit` reads before anything else.
static PARSER_DIRECTIVE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^#\s*([A-Za-z]+)\s*=\s*(\S+)\s*$"));

static LABEL_PAIR: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"([A-Za-z0-9._\-]+)=("(?:[^"\\]|\\.)*"|\S+)"#));

/// The value of the file's syntax parser directive. `BuildKit` reads directives only from the
/// file's leading lines, and stops at the first line that is not one (a comment, a blank line or
/// an instruction), so a `# syntax=` line anywhere later is an ordinary comment: this reader stops
/// at the same place, and reports the directive missing rather than find one `BuildKit` ignores.
#[must_use]
pub fn syntax(content: &str) -> Option<String> {
    for line in content.split('\n') {
        let captures = PARSER_DIRECTIVE.captures(line.trim())?;
        if captures[1].eq_ignore_ascii_case("syntax") {
            return Some(captures[2].to_owned());
        }
    }
    None
}

/// Splits a Dockerfile into instructions, dropping comments and blank lines and joining backslash
/// continuations, and groups them into stages.
///
/// # Errors
///
/// An instruction other than ARG before the first FROM, a FROM this reader cannot read, or a file
/// that ends inside a continuation.
pub fn parse(content: &str) -> Result<Vec<Stage>, Malformed> {
    let mut stages: Vec<Stage> = Vec::new();
    let mut pending = String::new();

    for line in content.split('\n') {
        let trimmed = line.trim();
        // A comment line is dropped even inside a continuation, as BuildKit does.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(body) = trimmed.strip_suffix('\\') {
            pending.push_str(body);
            pending.push(' ');
            continue;
        }

        let instruction = format!("{pending}{trimmed}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        pending.clear();
        let (keyword, args) = instruction
            .split_once(' ')
            .unwrap_or((instruction.as_str(), ""));
        let keyword = keyword.to_ascii_uppercase();

        if keyword == "FROM" {
            stages.push(parse_from(args)?);
            continue;
        }
        let Some(last) = stages.last_mut() else {
            if keyword == "ARG" {
                continue;
            }
            return Err(Malformed(format!("{keyword} before the first FROM")));
        };
        last.instructions.push(Instruction {
            keyword,
            args: args.to_owned(),
        });
    }

    if !pending.trim().is_empty() {
        return Err(Malformed("the file ends inside a continuation".to_owned()));
    }
    Ok(stages)
}

fn parse_from(args: &str) -> Result<Stage, Malformed> {
    let mut stage = Stage::default();
    let mut fields: Vec<&str> = args.split_whitespace().collect();
    while fields.first().is_some_and(|field| field.starts_with("--")) {
        if let Some(platform) = fields[0].strip_prefix("--platform=") {
            platform.clone_into(&mut stage.platform);
        }
        fields.remove(0);
    }
    match fields.as_slice() {
        [image] => (*image).clone_into(&mut stage.image),
        [image, alias, name] if alias.eq_ignore_ascii_case("AS") => {
            (*image).clone_into(&mut stage.image);
            (*name).clone_into(&mut stage.name);
        }
        _ => return Err(Malformed(format!("FROM {args}"))),
    }
    Ok(stage)
}

/// Whether a base reference is literal and pinned by a tag other than latest, without a digest.
#[must_use]
pub fn tag_pinned(image: &str) -> bool {
    TAG_PINNED.is_match(image) && repository(image) != image && tag(image) != "latest"
}

/// Whether a base reference is literal and pinned by digest.
#[must_use]
pub fn digest_pinned(image: &str) -> bool {
    DIGEST_PINNED.is_match(image)
}

/// The reference without its tag and digest: `dhi.io/rust` for
/// `dhi.io/rust:1.98.1-debian13-dev@sha256:...`.
#[must_use]
pub fn repository(image: &str) -> &str {
    let name = image.split_once('@').map_or(image, |(name, _)| name);
    let slash = name.rfind('/');
    match name.rfind(':') {
        Some(colon) if slash.is_none_or(|slash| colon > slash) => &name[..colon],
        Some(_) | None => name,
    }
}

/// The tag of a reference, empty when it has none.
#[must_use]
pub fn tag(image: &str) -> &str {
    let name = image.split_once('@').map_or(image, |(name, _)| name);
    let slash = name.rfind('/');
    match name.rfind(':') {
        Some(colon) if slash.is_none_or(|slash| colon > slash) => &name[colon + 1..],
        Some(_) | None => "",
    }
}

/// The LABEL key/value pairs of a stage, with the quotes around values removed.
#[must_use]
pub fn labels(stage: &Stage) -> BTreeMap<String, String> {
    stage
        .instructions
        .iter()
        .filter(|instruction| instruction.keyword == "LABEL")
        .flat_map(|instruction| {
            LABEL_PAIR
                .captures_iter(&instruction.args)
                .map(|captures| {
                    (
                        captures[1].to_owned(),
                        captures[2].trim_matches('"').to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
