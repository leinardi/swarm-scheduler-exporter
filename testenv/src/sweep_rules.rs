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

//! The sweep's decision rules and the owner labels, free of the Docker SDK and of the
//! `integration` feature, so their tests run in a plain `cargo test` with no daemon.

use std::fs;
use std::time::Duration;

use jiff::Timestamp;
use swarm_scheduler_exporter::go_compat::{duration, quote};

use crate::Error;

/// `<hostname>:<pid>` of the test process that created the resource.
pub const LABEL_OWNER: &str = "swarm-scheduler-exporter.it.owner";
/// When the environment came up, in RFC 3339.
pub const LABEL_STARTED: &str = "swarm-scheduler-exporter.it.started";
/// The PID namespace of the owner (`pid:[4026531836]`): a PID means something only inside its
/// namespace, and a test in a container that shares the host's hostname (`--network host`) has
/// its own.
pub const LABEL_PID_NAMESPACE: &str = "swarm-scheduler-exporter.it.pidns";

/// Overrides how old an abandoned environment must be before the sweep removes it.
pub const ENV_STALE_AFTER: &str = "SSE_IT_STALE_AFTER";
/// The age below which no other environment is ever removed.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(3600);

const NANOS_PER_MINUTE: i64 = 60_000_000_000;

/// The process that created an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// The hostname.
    pub host: String,
    /// The PID, positive.
    pub pid: i64,
    /// The namespace the PID lives in, empty where it cannot be read.
    pub pid_namespace: String,
}

impl std::fmt::Display for Owner {
    /// The [`LABEL_OWNER`] value; the namespace has a label of its own.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.host, self.pid)
    }
}

/// This process on this host.
///
/// # Errors
///
/// The hostname cannot be read.
pub fn current_owner() -> Result<Owner, Error> {
    Ok(Owner {
        host: hostname()?,
        pid: i64::from(std::process::id()),
        pid_namespace: current_pid_namespace(),
    })
}

/// Go's `os.Hostname` on Linux: the kernel's hostname, as `/proc` reports it.
fn hostname() -> Result<String, Error> {
    let raw = fs::read_to_string("/proc/sys/kernel/hostname")
        .map_err(|error| Error::new(format!("hostname: {error}")))?;
    Ok(raw.strip_suffix('\n').unwrap_or(&raw).to_owned())
}

/// This process's PID namespace, or "" where `/proc` does not say. An empty namespace never
/// matches, so an environment from such a process is never swept by PID.
#[must_use]
pub fn current_pid_namespace() -> String {
    fs::read_link("/proc/self/ns/pid")
        .map(|target| target.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads an owner label value. The PID is after the last colon, so a hostname may hold colons
/// of its own.
///
/// # Errors
///
/// No colon, nothing before it, or a PID that is not a positive integer (Go's `strconv.Atoi`).
pub fn parse_owner(value: &str) -> Result<Owner, Error> {
    let bad = || {
        Error::new(format!(
            "owner is not <hostname>:<pid>: {}",
            quote::quote(value)
        ))
    };
    let colon = value
        .rfind(':')
        .filter(|&colon| colon > 0)
        .ok_or_else(bad)?;
    let pid: i64 = value[colon + 1..].parse().map_err(|_| bad())?;
    if pid <= 0 {
        return Err(bad());
    }
    Ok(Owner {
        host: value[..colon].to_owned(),
        pid,
        pid_namespace: String::new(),
    })
}

/// One environment as its labels describe it. The owner and started labels are taken from
/// whichever of its resources carries them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// The envid.
    pub id: String,
    /// The [`LABEL_OWNER`] value.
    pub owner: String,
    /// The [`LABEL_STARTED`] value.
    pub started: String,
    /// The [`LABEL_PID_NAMESPACE`] value.
    pub pid_namespace: String,
}

/// The sweep's decision about one environment, with the reason a reader is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The environment decided on.
    pub env: Found,
    /// Whether the sweep removes it.
    pub remove: bool,
    /// Why.
    pub reason: String,
}

/// Decides which environments are abandoned. Another envid is removed only when all of these
/// hold: it is older than `stale_after`, and its owner is provably dead -- on this host, in this
/// PID namespace, and its PID does not exist. A foreign host is never assumed dead, and a label
/// that cannot be read proves nothing: such an environment is left for `make sweep-test-leaks`.
pub struct SweepRules {
    /// The time the ages are measured at.
    pub now: Timestamp,
    /// The age below which nothing is removed.
    pub stale_after: Duration,
    /// This host.
    pub host: String,
    /// This process's PID namespace; empty matches nothing.
    pub pid_namespace: String,
    /// The current envid, which a sweep never touches; `down` owns it.
    pub self_id: String,
    /// Whether a PID exists on this host.
    pub alive: Box<dyn Fn(i64) -> bool + Send + Sync>,
}

impl SweepRules {
    /// Applies the rules to one environment.
    #[must_use]
    pub fn decide(&self, env: Found) -> Verdict {
        let keep = |env: Found, reason: String| Verdict {
            env,
            remove: false,
            reason,
        };

        if env.id == self.self_id {
            return keep(env, "the current environment".to_owned());
        }

        // jiff reads RFC 3339 as Go's time.Parse does for every value the harness writes; it is
        // laxer about case and the separator, which no label written here uses.
        let Ok(started) = env.started.parse::<Timestamp>() else {
            return keep(env, "its started label is unreadable".to_owned());
        };

        let age = saturating_nanos(self.now.duration_since(started).as_nanos());
        let stale_after = i64::try_from(self.stale_after.as_nanos()).unwrap_or(i64::MAX);
        if age < stale_after {
            return keep(
                env,
                format!(
                    "started {} ago, under {}",
                    duration::format(round(age, NANOS_PER_MINUTE)),
                    duration::format(stale_after)
                ),
            );
        }

        let Ok(owner) = parse_owner(&env.owner) else {
            return keep(env, "its owner label is unreadable".to_owned());
        };

        if owner.host != self.host {
            return keep(env, format!("owned by another host, {}", owner.host));
        }

        if self.pid_namespace.is_empty() || env.pid_namespace != self.pid_namespace {
            return keep(
                env,
                "owned by a process in another or unknown PID namespace".to_owned(),
            );
        }

        if (self.alive)(owner.pid) {
            return keep(
                env,
                format!("its owner, PID {}, is still running", owner.pid),
            );
        }

        Verdict {
            env,
            remove: true,
            reason: format!("stale, and its owner PID {} is gone", owner.pid),
        }
    }
}

/// Reads [`ENV_STALE_AFTER`] through `getenv`, defaulting to [`DEFAULT_STALE_AFTER`].
///
/// # Errors
///
/// A value that is not a positive duration, rather than a default: a typo must not shorten the
/// window.
pub fn stale_after_from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Duration, Error> {
    let raw = getenv(ENV_STALE_AFTER).unwrap_or_default();
    if raw.is_empty() {
        return Ok(DEFAULT_STALE_AFTER);
    }
    match duration::parse(&raw) {
        Ok(nanos) if nanos > 0 => Ok(Duration::from_nanos(nanos.unsigned_abs())),
        Ok(_) | Err(_) => Err(Error::new(format!(
            "bad stale-after window: {ENV_STALE_AFTER}={} is not a positive duration",
            quote::quote(&raw)
        ))),
    }
}

fn saturating_nanos(nanos: i128) -> i64 {
    i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX })
}

/// Go's `Duration.Round`: to the nearest multiple of `multiple`, halfway values away from zero,
/// saturating at the ends of the range.
fn round(nanos: i64, multiple: i64) -> i64 {
    if multiple <= 0 {
        return nanos;
    }
    let less_than_half = |remainder: i64| remainder.unsigned_abs() * 2 < multiple.unsigned_abs();
    let remainder = nanos % multiple;
    if nanos < 0 {
        let remainder = -remainder;
        if less_than_half(remainder) {
            return nanos + remainder;
        }
        return nanos.checked_sub(multiple - remainder).unwrap_or(i64::MIN);
    }
    if less_than_half(remainder) {
        return nanos - remainder;
    }
    nanos.checked_add(multiple - remainder).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
