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

//! The exporter's logger: slog's text and JSON formats and the exporter's plain format, byte for
//! byte.
//!
//! A [`Logger`] carries a minimum [`Level`], a [`Format`], whether the time is logged, a sink and
//! the clock and zone the record time is read from. `with_attrs` and `with_group` return a copy;
//! the original is unchanged. Each record is formatted whole, then written under one lock, so
//! records never interleave.
//!
//! The levels are slog's: debug -4, info 0, warn 4, error 8, and any value between (`INFO+1`).

mod plain_handler;
mod slog_handler;
mod value;

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use jiff::tz::TimeZone;

use crate::clock::{Clock, SystemClock};
use crate::gofmt_compat::{local_zone, time_layout};

use plain_handler::PlainHandler;
use slog_handler::SlogHandler;
pub use value::{Attr, Time, Value};

/// A slog level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(pub i32);

impl Level {
    /// `slog.LevelDebug`.
    pub const DEBUG: Self = Self(-4);
    /// `slog.LevelInfo`.
    pub const INFO: Self = Self(0);
    /// `slog.LevelWarn`.
    pub const WARN: Self = Self(4);
    /// `slog.LevelError`.
    pub const ERROR: Self = Self(8);
}

impl fmt::Display for Level {
    /// slog's `Level.String`: `INFO`, `INFO+1`, `DEBUG-1`, `ERROR+4`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (base, delta) = if *self < Self::INFO {
            ("DEBUG", self.0 - Self::DEBUG.0)
        } else if *self < Self::WARN {
            ("INFO", self.0 - Self::INFO.0)
        } else if *self < Self::ERROR {
            ("WARN", self.0 - Self::WARN.0)
        } else {
            ("ERROR", self.0 - Self::ERROR.0)
        };
        if delta == 0 {
            formatter.write_str(base)
        } else {
            write!(formatter, "{base}{delta:+}")
        }
    }
}

/// The log format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// slog's `TextHandler`.
    Text,
    /// slog's `JSONHandler`.
    Json,
    /// The exporter's plain handler.
    Plain,
}

/// A `--log-level` or `--log-format` value that is not one of the accepted ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    what: &'static str,
    value: String,
    accepted: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid log {} {:?} (want one of {})",
            self.what, self.value, self.accepted
        )
    }
}

impl std::error::Error for ParseError {}

/// A log level name, in any case: `debug`, `info`, `warn` or `warning`, and `error`, `fatal` or
/// `panic` (both read as error).
///
/// # Errors
///
/// Any other value (Q4: Go silently fell back to info).
pub fn parse_level(value: &str) -> Result<Level, ParseError> {
    match go_to_lower(value).as_str() {
        "debug" => Ok(Level::DEBUG),
        "info" => Ok(Level::INFO),
        "warn" | "warning" => Ok(Level::WARN),
        "error" | "fatal" | "panic" => Ok(Level::ERROR),
        _ => Err(ParseError {
            what: "level",
            value: value.to_owned(),
            accepted: "debug, info, warn, warning, error, fatal, panic",
        }),
    }
}

/// A log format name, in any case: `json`, `text` or `plain`.
///
/// # Errors
///
/// Any other value (Q4: Go silently fell back to text).
pub fn parse_format(value: &str) -> Result<Format, ParseError> {
    match go_to_lower(value).as_str() {
        "json" => Ok(Format::Json),
        "text" => Ok(Format::Text),
        "plain" => Ok(Format::Plain),
        _ => Err(ParseError {
            what: "format",
            value: value.to_owned(),
            accepted: "json, text, plain",
        }),
    }
}

/// Go's `strings.ToLower`, which maps each character to one character: it differs from Rust's
/// `to_lowercase` on U+0130 (`İ`), which Go lowers to a plain `i` and Rust to `i` plus a
/// combining dot, so `İNFO` is a level in Go.
fn go_to_lower(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            let lower: Vec<char> = if character == '\u{130}' {
                vec!['i']
            } else {
                character.to_lowercase().collect()
            };
            lower
        })
        .collect()
}

/// Where the record time comes from.
pub type Now = Arc<dyn Clock>;

/// A sink every record is written to, whole, under one lock.
pub type Sink = Box<dyn Write + Send>;

/// The logger options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The format.
    pub format: Format,
    /// The minimum level written.
    pub level: Level,
    /// Whether records carry their time (`--log-time`).
    pub include_time: bool,
}

/// What every copy of a logger shares.
struct Shared {
    level: Level,
    include_time: bool,
    sink: Mutex<Sink>,
    zone: TimeZone,
    now: Now,
}

/// The handler a logger formats with.
#[derive(Debug, Clone, PartialEq)]
enum Handler {
    Slog(SlogHandler),
    Plain(PlainHandler),
}

/// A logger. Clones are cheap and share the sink.
#[derive(Clone)]
pub struct Logger {
    shared: Arc<Shared>,
    handler: Arc<Handler>,
}

impl fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Logger")
            .field("level", &self.shared.level)
            .field("include_time", &self.shared.include_time)
            .field("handler", &self.handler)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// A logger writing to `sink`, with record times from `now` written in `zone`.
    #[must_use]
    pub fn new(config: Config, sink: Sink, zone: TimeZone, now: Now) -> Self {
        let handler = match config.format {
            Format::Text => Handler::Slog(SlogHandler::new(false, config.include_time)),
            Format::Json => Handler::Slog(SlogHandler::new(true, config.include_time)),
            Format::Plain => Handler::Plain(PlainHandler::default()),
        };
        Self {
            shared: Arc::new(Shared {
                level: config.level,
                include_time: config.include_time,
                sink: Mutex::new(sink),
                zone,
                now,
            }),
            handler: Arc::new(handler),
        }
    }

    /// The zone record times are written in, Go's `time.Local`.
    #[must_use]
    pub fn zone(&self) -> &TimeZone {
        &self.shared.zone
    }

    /// Whether a record at `level` is written.
    #[must_use]
    pub fn enabled(&self, level: Level) -> bool {
        level >= self.shared.level
    }

    /// `Logger.With`: a copy that adds `attrs` to every record. No attributes return the same
    /// logger.
    #[must_use]
    pub fn with_attrs(&self, attrs: &[Attr]) -> Self {
        if attrs.is_empty() {
            return self.clone();
        }
        let handler = match &*self.handler {
            Handler::Slog(handler) => Handler::Slog(handler.with_attrs(attrs)),
            Handler::Plain(handler) => Handler::Plain(handler.with_attrs(attrs)),
        };
        self.with_handler(handler)
    }

    /// `Logger.WithGroup`: a copy that qualifies the attributes that follow with `name`. An empty
    /// name returns the same logger (and the plain format also ignores a blank one).
    #[must_use]
    pub fn with_group(&self, name: &str) -> Self {
        if name.is_empty() {
            return self.clone();
        }
        let handler = match &*self.handler {
            Handler::Slog(handler) => Handler::Slog(handler.with_group(name)),
            Handler::Plain(handler) => Handler::Plain(handler.with_group(name)),
        };
        self.with_handler(handler)
    }

    fn with_handler(&self, handler: Handler) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            handler: Arc::new(handler),
        }
    }

    /// Logs at debug.
    pub fn debug(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::DEBUG, message, attrs);
    }

    /// Logs at info.
    pub fn info(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::INFO, message, attrs);
    }

    /// Logs at warn.
    pub fn warn(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::WARN, message, attrs);
    }

    /// Logs at error.
    pub fn error(&self, message: &str, attrs: &[Attr]) {
        self.log(Level::ERROR, message, attrs);
    }

    /// Logs one record at `level`, if enabled. Empty groups among `attrs` are dropped, as
    /// `Record.Add` does. A failed write has nowhere to be reported and is ignored, as slog's
    /// `Logger` ignores its handler's error.
    pub fn log(&self, level: Level, message: &str, attrs: &[Attr]) {
        if !self.enabled(level) {
            return;
        }
        let timestamp = self.shared.now.now();
        let time = (!time_layout::is_go_zero_time(timestamp)).then(|| Time {
            timestamp,
            offset: self.shared.zone.to_offset(timestamp),
        });
        let attrs: Vec<Attr> = attrs
            .iter()
            .filter(|attr| !attr.is_empty_group())
            .cloned()
            .collect();
        let line = match &*self.handler {
            Handler::Slog(handler) => handler.format(time, level, message, &attrs),
            // The plain handler checks the time option itself; slog's handlers drop the time
            // through the time stripper.
            Handler::Plain(handler) => {
                let time = time.filter(|_| self.shared.include_time);
                handler.format(time, level, message, &attrs)
            }
        };
        let mut sink = self
            .shared
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = sink.write_all(line.as_bytes());
        let _ = sink.flush();
    }
}

/// A sink that keeps what is written, for tests: clones share the buffer.
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl MemorySink {
    /// Everything written so far, as text.
    #[must_use]
    pub fn contents(&self) -> String {
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

impl Write for MemorySink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The process-wide logger, set by [`configure`] or [`set_global`].
static GLOBAL: RwLock<Option<Logger>> = RwLock::new(None);

/// The process-wide logger. Before [`configure`] it is Go's default: text at info, with the
/// system clock's time in the local zone, to stdout.
#[must_use]
pub fn global() -> Logger {
    if let Some(logger) = GLOBAL
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return logger.clone();
    }
    let mut global = GLOBAL.write().unwrap_or_else(PoisonError::into_inner);
    global
        .get_or_insert_with(|| {
            Logger::new(
                Config {
                    format: Format::Text,
                    level: Level::INFO,
                    include_time: true,
                },
                Box::new(io::stdout()),
                local_zone::local_zone(|key| std::env::var(key).ok()),
                Arc::new(SystemClock),
            )
        })
        .clone()
}

/// Replaces the process-wide logger.
pub fn set_global(logger: Logger) {
    *GLOBAL.write().unwrap_or_else(PoisonError::into_inner) = Some(logger);
}

/// Builds the logger the flags ask for, writing to stdout with record times from `now` in the
/// local zone (resolved from `getenv("TZ")` as Go does), and installs it as the global one.
pub fn configure(config: Config, now: Now, getenv: impl Fn(&str) -> Option<String>) -> Logger {
    let logger = Logger::new(
        config,
        Box::new(io::stdout()),
        local_zone::local_zone(getenv),
        now,
    );
    set_global(logger.clone());
    logger
}

#[cfg(test)]
mod tests;
