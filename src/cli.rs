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

//! The command line: Go's flag set, double-dash only (Q3), with the log flags refusing unknown
//! values (Q4).
//!
//! Go's `flag` read `-name` and `--name` alike, ignored positional arguments and read
//! `-containers false` as true plus an ignored `false`. Here only `--name` is accepted, and
//! [`parse`] refuses every single-dash argument longer than one character before clap sees it:
//! clap alone would read `-unknown` as the short cluster `-u` and print the help for `-hx`.
//! Kept from Go: the names, defaults and help texts, last value wins, `--containers=false` and
//! every `strconv.ParseBool` spelling, Go's duration syntax, a repeatable `--label` refusing an
//! empty value, and exit 2 on a parse error.

use std::ffi::OsString;
use std::fmt;

use clap::{ArgAction, Parser};

use crate::go_compat::{duration, quote};
use crate::labels::{self, CountError, LabelError};
use crate::logger::{self, Format, Level};

/// Go's `DefaultPollDelay`: 10s.
pub const DEFAULT_POLL_DELAY: i64 = 10_000_000_000;

/// Go's `minPollDelay`: 1s.
pub const MIN_POLL_DELAY: i64 = 1_000_000_000;

/// The label names the exporter already sets on its per-service metrics, which no custom label
/// may take (Go's `collector.ReservedLabelNames`).
pub const RESERVED_LABEL_NAMES: [&str; 5] =
    ["stack", "service", "service_mode", "display_name", "state"];

/// The flags.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per Go bool flag: the struct is the flag set, not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "swarm-scheduler-exporter",
    bin_name = "swarm-scheduler-exporter",
    // Last value wins, as with Go's flag package.
    args_override_self = true,
    disable_version_flag = true,
    // Go's -help text replaces clap's.
    disable_help_flag = true,
    // The help is the flag list only, as Go's was; not this type's doc comment.
    about = None,
    long_about = None
)]
pub struct Args {
    #[arg(
        long,
        default_value = "0.0.0.0:8888",
        allow_hyphen_values = true,
        value_name = "string",
        help = "IP address and port to bind"
    )]
    pub listen_addr: String,

    /// The poll interval, in nanoseconds, as Go's `time.Duration` holds it.
    #[arg(
        long,
        default_value = "10s",
        value_parser = duration::parse,
        allow_hyphen_values = true,
        value_name = "duration",
        help = "How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s."
    )]
    pub poll_delay: i64,

    #[arg(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_value = "false",
        default_missing_value = "true",
        value_parser = parse_bool,
        value_name = "BOOL",
        hide_default_value = true,
        help = "Expose container state metrics (opt-in)."
    )]
    pub containers: bool,

    #[arg(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_value = "false",
        default_missing_value = "true",
        value_parser = parse_bool,
        value_name = "BOOL",
        hide_default_value = true,
        help = "Include containers belonging to Swarm tasks."
    )]
    pub containers_include_swarm: bool,

    #[arg(
        long,
        default_value = "text",
        value_parser = logger::parse_format,
        allow_hyphen_values = true,
        value_name = "string",
        help = "Either json, text or plain"
    )]
    pub log_format: Format,

    #[arg(
        long,
        default_value = "info",
        value_parser = logger::parse_level,
        allow_hyphen_values = true,
        value_name = "string",
        help = "Either debug, info, warn, error, fatal, panic"
    )]
    pub log_level: Level,

    #[arg(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_value = "false",
        default_missing_value = "true",
        value_parser = parse_bool,
        value_name = "BOOL",
        hide_default_value = true,
        help = "Include timestamp in logs"
    )]
    pub log_time: bool,

    #[arg(
        long,
        action = ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_value = "false",
        default_missing_value = "true",
        value_parser = parse_bool,
        value_name = "BOOL",
        hide_default_value = true,
        help = "Probe this exporter's /healthz on --listen-addr and exit 0 if healthy, 1 otherwise"
    )]
    pub healthcheck: bool,

    /// The custom labels, in the order given.
    #[arg(
        long = "label",
        action = ArgAction::Append,
        value_parser = parse_label,
        allow_hyphen_values = true,
        value_name = "value",
        help = "Name of custom service labels to add to metrics"
    )]
    pub labels: Vec<String>,

    #[arg(short = 'h', long, action = ArgAction::Help, help = "Display help message")]
    help: Option<bool>,
}

/// The flags that take a separate value: the argument after one of them, without `=`, is its
/// value, even when it starts with a dash (`--label -poll-delay`).
const VALUE_FLAGS: [&str; 5] = [
    "listen-addr",
    "poll-delay",
    "log-format",
    "log-level",
    "label",
];

/// The flags that take no separate value.
const BOOL_FLAGS: [&str; 5] = [
    "containers",
    "containers-include-swarm",
    "log-time",
    "healthcheck",
    "help",
];

/// Why parsing stopped: the text to print, where, and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// The exit code: 0 after the help, 2 for a parse error.
    pub code: i32,
    /// Whether the text goes to stdout (the help) rather than stderr.
    pub stdout: bool,
    /// The text, newline included.
    pub message: String,
}

/// Parses the arguments, the program name first.
///
/// # Errors
///
/// [`Exit`] after the help (code 0), or for a single-dash flag, an unknown flag, a positional
/// argument or an invalid value (code 2).
pub fn parse<I, T>(arguments: I) -> Result<Args, Exit>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let arguments: Vec<OsString> = arguments.into_iter().map(Into::into).collect();
    if let Err(message) = refuse_single_dash(&arguments) {
        return Err(Exit {
            code: 2,
            stdout: false,
            message: format!("error: {message}\n"),
        });
    }
    Args::try_parse_from(arguments).map_err(|error| Exit {
        code: error.exit_code(),
        stdout: !error.use_stderr(),
        message: error.render().to_string(),
    })
}

/// The single-dash pre-scan: every argument up to `--` that starts with one dash and is longer
/// than one character is refused, naming the `--` spelling when it is a known flag. The value
/// after a value flag without `=` is skipped, since it may start with a dash.
fn refuse_single_dash(arguments: &[OsString]) -> Result<(), String> {
    let mut skip_value = false;
    for argument in arguments.iter().skip(1) {
        if skip_value {
            skip_value = false;
            continue;
        }
        let argument = argument.to_string_lossy();
        if argument == "--" {
            break;
        }
        if let Some(rest) = argument.strip_prefix("--") {
            skip_value = !rest.contains('=') && VALUE_FLAGS.contains(&rest);
            continue;
        }
        let Some(rest) = argument.strip_prefix('-') else {
            continue;
        };
        let name = rest.split_once('=').map_or(rest, |(name, _)| name);
        if VALUE_FLAGS.contains(&name) || BOOL_FLAGS.contains(&name) {
            return Err(format!(
                "single-dash long flags are not supported: '-{name}'; use '--{name}'"
            ));
        }
        // -h is the only short flag; anything longer, a value after = included (clap would run
        // -h=false as the help), is a Go-style flag.
        if rest.chars().count() > 1 {
            return Err(format!("flag provided but not defined: '-{name}'"));
        }
    }
    Ok(())
}

/// The value of a bool flag: exactly `strconv.ParseBool`'s spellings.
fn parse_bool(value: &str) -> Result<bool, &'static str> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err("parse error"),
    }
}

/// The error of an empty `--label`, worded as Go's `ErrEmptyFlagValue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyFlagValue;

impl fmt::Display for EmptyFlagValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("empty flag value")
    }
}

impl std::error::Error for EmptyFlagValue {}

/// Go's `stringSlice.Set`: any value but the empty one.
fn parse_label(value: &str) -> Result<String, EmptyFlagValue> {
    if value.is_empty() {
        return Err(EmptyFlagValue);
    }
    Ok(value.to_owned())
}

/// The error of a poll delay below [`MIN_POLL_DELAY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollDelayTooShort;

impl fmt::Display for PollDelayTooShort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("poll-delay must be >= ")?;
        formatter.write_str(&duration::format(MIN_POLL_DELAY))
    }
}

impl std::error::Error for PollDelayTooShort {}

/// Go's poll-delay check, run after `--healthcheck` has had its turn (exit 1).
///
/// # Errors
///
/// [`PollDelayTooShort`] below 1s.
pub fn check_poll_delay(poll_delay: i64) -> Result<(), PollDelayTooShort> {
    if poll_delay < MIN_POLL_DELAY {
        return Err(PollDelayTooShort);
    }
    Ok(())
}

/// A custom label: the Docker service label key it reads, and the Prometheus label name it is
/// exported as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomLabel {
    /// The key as given on the command line.
    pub raw: String,
    /// The sanitized Prometheus label name.
    pub sanitized: String,
}

/// Why the custom labels were refused, worded as Go's `validateAndSetCustomLabels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomLabelsError {
    /// More labels than allowed.
    Count(CountError),
    /// A label name that is invalid once sanitized, or two that sanitize alike.
    Sanitize(LabelError),
    /// A label that sanitizes to a name the exporter already uses.
    Reserved(String),
}

impl fmt::Display for CustomLabelsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(error) => write!(formatter, "validate custom label count: {error}"),
            Self::Sanitize(error) => write!(formatter, "sanitize custom label names: {error}"),
            Self::Reserved(name) => write!(
                formatter,
                "custom label collides with a reserved label name: {}",
                quote::quote(name)
            ),
        }
    }
}

impl std::error::Error for CustomLabelsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Count(error) => Some(error),
            Self::Sanitize(error) => Some(error),
            Self::Reserved(_) => None,
        }
    }
}

/// Go's `validateAndSetCustomLabels`: the count first, then each name sanitized and validated,
/// then the reserved names. Go stored the result in a collector global; here it is returned, for
/// the collectors to be built with.
///
/// # Errors
///
/// [`CustomLabelsError`], at the first check that fails.
pub fn validate_custom_labels(raw_keys: &[String]) -> Result<Vec<CustomLabel>, CustomLabelsError> {
    labels::validate_custom_label_count(raw_keys.len()).map_err(CustomLabelsError::Count)?;
    let sanitized =
        labels::validate_and_sanitize_label_names(raw_keys).map_err(CustomLabelsError::Sanitize)?;
    if let Some(name) = sanitized
        .iter()
        .find(|name| RESERVED_LABEL_NAMES.contains(&name.as_str()))
    {
        return Err(CustomLabelsError::Reserved(name.clone()));
    }
    Ok(raw_keys
        .iter()
        .zip(sanitized)
        .map(|(raw, sanitized)| CustomLabel {
            raw: raw.clone(),
            sanitized,
        })
        .collect())
}

#[cfg(test)]
mod tests;
