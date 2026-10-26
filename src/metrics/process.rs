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

//! The Prometheus Go client's process collector on Linux (v1.24.1, over procfs v0.21.1), read
//! in-tree from `/proc`: the nine `process_*` families with Go's names, types and help texts (Q1).
//!
//! Each value comes from where procfs reads it, with procfs's quirks:
//!
//! - CPU time and the start time count in procfs's hard-coded 100 ticks per second, not the
//!   system's `CLK_TCK`;
//! - RSS is the page count in `/proc/self/stat` times the page size, which Go's runtime takes from
//!   the `AT_PAGESZ` entry of `/proc/self/auxv`, as here;
//! - `open_fds` is the `st_size` of `/proc/self/fd` (Linux 6.2 and later report the descriptor
//!   count there), counting the entries only where it is 0 or the tree is not the real `/proc`;
//! - the network counters are `IpExt` `InOctets` and `OutOctets` of `/proc/self/net/netstat`, which
//!   count the whole network namespace, not the process; 0 when the file has no such line.
//!
//! The page size is read once; without an `AT_PAGESZ` entry, which the kernel always provides
//! and Go's runtime needs to start, RSS is left out rather than guessed.
//!
//! Errors are not reported, as with the Prometheus Go client's default `ReportErrors: false`: a
//! file that cannot be read or parsed drops the families it feeds from that scrape (`stat` the CPU,
//! memory and start time; `/proc/stat` the start time only; `fd`, `limits` and `net/netstat`
//! theirs).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::go_compat::float;

use super::{Collector, Desc, Family, Kind, Series, Value, constant_desc};

/// procfs's `userHZ`.
const USER_HZ: f64 = 100.0;

/// procfs's value for `unlimited` in `/proc/self/limits`.
const UNLIMITED: u64 = u64::MAX;

/// The process collector's descriptors, in the Prometheus Go client's order.
#[derive(Debug)]
struct Descs {
    cpu_total: Arc<Desc>,
    open_fds: Arc<Desc>,
    max_fds: Arc<Desc>,
    vsize: Arc<Desc>,
    max_vsize: Arc<Desc>,
    rss: Arc<Desc>,
    start_time: Arc<Desc>,
    in_bytes: Arc<Desc>,
    out_bytes: Arc<Desc>,
}

/// The Prometheus Go client's `NewProcessCollector` with no namespace.
#[derive(Debug)]
pub struct ProcessCollector {
    root: PathBuf,
    // procfs's isReal: the fd fast path only applies to the real /proc.
    real: bool,
    descs: Descs,
}

fn desc(name: &str, help: &str, kind: Kind) -> Arc<Desc> {
    constant_desc(name, help, kind, &[])
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCollector {
    /// The collector of this process, reading `/proc`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_root(Path::new("/proc"), true)
    }

    /// The collector reading a `/proc`-like tree at `root`, for tests; `real` says whether it is
    /// the kernel's `/proc`.
    fn with_root(root: &Path, real: bool) -> Self {
        Self {
            root: root.to_owned(),
            real,
            descs: Descs {
                cpu_total: desc(
                    "process_cpu_seconds_total",
                    "Total user and system CPU time spent in seconds.",
                    Kind::Counter,
                ),
                open_fds: desc(
                    "process_open_fds",
                    "Number of open file descriptors.",
                    Kind::Gauge,
                ),
                max_fds: desc(
                    "process_max_fds",
                    "Maximum number of open file descriptors.",
                    Kind::Gauge,
                ),
                vsize: desc(
                    "process_virtual_memory_bytes",
                    "Virtual memory size in bytes.",
                    Kind::Gauge,
                ),
                max_vsize: desc(
                    "process_virtual_memory_max_bytes",
                    "Maximum amount of virtual memory available in bytes.",
                    Kind::Gauge,
                ),
                rss: desc(
                    "process_resident_memory_bytes",
                    "Resident memory size in bytes.",
                    Kind::Gauge,
                ),
                start_time: desc(
                    "process_start_time_seconds",
                    "Start time of the process since unix epoch in seconds.",
                    Kind::Gauge,
                ),
                in_bytes: desc(
                    "process_network_receive_bytes_total",
                    "Number of bytes received by the process over the network.",
                    Kind::Counter,
                ),
                out_bytes: desc(
                    "process_network_transmit_bytes_total",
                    "Number of bytes sent by the process over the network.",
                    Kind::Counter,
                ),
            },
        }
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.root.join(path)).ok()
    }

    /// procfs's `FileDescriptorsLen`.
    fn open_fds(&self) -> Option<u64> {
        let path = self.root.join("self/fd");
        if self.real {
            let size = fs::metadata(&path).ok()?.len();
            if size > 0 {
                return Some(size);
            }
        }
        let mut count = 0;
        for entry in fs::read_dir(&path).ok()? {
            entry.ok()?;
            count += 1;
        }
        Some(count)
    }
}

fn push(families: &mut Vec<Family>, desc: &Arc<Desc>, value: f64) {
    families.push(Family {
        desc: Arc::clone(desc),
        series: vec![Series {
            label_values: Vec::new(),
            value: Value::Scalar(value),
        }],
    });
}

impl Collector for ProcessCollector {
    fn descs(&self) -> Vec<Arc<Desc>> {
        let descs = &self.descs;
        [
            &descs.cpu_total,
            &descs.open_fds,
            &descs.max_fds,
            &descs.vsize,
            &descs.max_vsize,
            &descs.rss,
            &descs.start_time,
            &descs.in_bytes,
            &descs.out_bytes,
        ]
        .into_iter()
        .map(Arc::clone)
        .collect()
    }

    fn collect(&self, families: &mut Vec<Family>) {
        let descs = &self.descs;
        if let Some(stat) = self.read("self/stat").as_deref().and_then(parse_stat) {
            let ticks = stat.utime.wrapping_add(stat.stime);
            push(families, &descs.cpu_total, float::from_int(ticks) / USER_HZ);
            push(families, &descs.vsize, float::from_int(stat.vsize));
            if let Some(page_size) = page_size() {
                // Go's int multiplication, which wraps.
                let rss = stat.rss.wrapping_mul(page_size);
                push(families, &descs.rss, float::from_int(rss));
            }
            if let Some(boot_time) = self.read("stat").as_deref().and_then(parse_boot_time) {
                let start =
                    float::from_int(boot_time) + float::from_int(stat.start_ticks) / USER_HZ;
                push(families, &descs.start_time, start);
            }
        }
        if let Some(open_fds) = self.open_fds() {
            push(families, &descs.open_fds, float::from_int(open_fds));
        }
        if let Some(limits) = self.read("self/limits").as_deref().and_then(parse_limits) {
            push(families, &descs.max_fds, float::from_int(limits.open_files));
            push(
                families,
                &descs.max_vsize,
                float::from_int(limits.address_space),
            );
        }
        if let Some((in_octets, out_octets)) = self
            .read("self/net/netstat")
            .as_deref()
            .and_then(parse_netstat)
        {
            push(families, &descs.in_bytes, in_octets.unwrap_or(0.0));
            push(families, &descs.out_bytes, out_octets.unwrap_or(0.0));
        }
    }
}

/// The page size: `AT_PAGESZ` (6) in `/proc/self/auxv`, native-endian word pairs, read once.
fn page_size() -> Option<i64> {
    static PAGE_SIZE: OnceLock<Option<i64>> = OnceLock::new();
    *PAGE_SIZE.get_or_init(|| {
        fs::read("/proc/self/auxv")
            .ok()
            .and_then(|auxv| auxv_page_size(&auxv))
    })
}

/// `AT_PAGESZ` among `auxv`'s `(key, value)` words; the vector ends at `AT_NULL` (0).
fn auxv_page_size(auxv: &[u8]) -> Option<i64> {
    const AT_NULL: usize = 0;
    const AT_PAGESZ: usize = 6;
    let width = size_of::<usize>();
    for pair in auxv.chunks_exact(width * 2) {
        let (key, value) = pair.split_at(width);
        let key = usize::from_ne_bytes(key.try_into().ok()?);
        let value = usize::from_ne_bytes(value.try_into().ok()?);
        match key {
            AT_NULL => return None,
            AT_PAGESZ => return i64::try_from(value).ok(),
            _ => {}
        }
    }
    None
}

/// The fields of `/proc/self/stat` the collector uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    utime: u64,
    stime: u64,
    start_ticks: u64,
    vsize: u64,
    rss: i64,
}

/// The Go types procfs's `fmt.Fscan` reads the 41 numeric fields after the state into: `i` for an
/// `int` or `int64`, `u` for a `uint` or `uint64`. A field that does not parse as its type fails
/// the whole read, as `Fscan` does.
const STAT_FIELD_TYPES: &[u8; 41] = b"iiiiiuuuuuuuiiiiiiuuiuuuuuuuuuuuuuiuuuuii";

/// procfs's `Proc.Stat`: the fields after the last `)`, which ends the command name (itself free
/// to hold spaces and parentheses), then the state and 41 numbers.
fn parse_stat(data: &str) -> Option<Stat> {
    data.find('(')?;
    let close = data.rfind(')')?;
    let mut fields = data.get(close + 2..)?.split_ascii_whitespace();
    fields.next()?;
    let mut values = [0_i128; 41];
    for (value, kind) in values.iter_mut().zip(STAT_FIELD_TYPES) {
        let field = fields.next()?;
        *value = if *kind == b'u' {
            field.parse::<u64>().ok()?.into()
        } else {
            field.parse::<i64>().ok()?.into()
        };
    }
    // Field positions after the state: utime 11, stime 12, starttime 19, vsize 20, rss 21.
    Some(Stat {
        utime: u64::try_from(values[10]).ok()?,
        stime: u64::try_from(values[11]).ok()?,
        start_ticks: u64::try_from(values[18]).ok()?,
        vsize: u64::try_from(values[19]).ok()?,
        rss: i64::try_from(values[20]).ok()?,
    })
}

/// The `btime` line of `/proc/stat`, as procfs's `FS.Stat` reads it: each line split on
/// whitespace, the value a decimal `uint64`; 0 without such a line, and a parse error fails it.
/// procfs also parses the `cpu`, `intr`, `ctxt`, `processes`, `procs_*` and `softirq` lines and
/// fails on a malformed one; the kernel writes them all as numbers, so they are not read here.
fn parse_boot_time(data: &str) -> Option<u64> {
    let mut boot_time = 0;
    for line in data.lines() {
        let parts: Vec<&str> = line.split_ascii_whitespace().collect();
        // procfs skips a line with fewer than two fields, and a later btime line wins.
        if parts.len() >= 2 && parts[0] == "btime" {
            boot_time = parse_go_uint(parts[1])?;
        }
    }
    Some(boot_time)
}

/// `strconv.ParseUint(s, 10, 64)`: decimal digits only, no sign.
fn parse_go_uint(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The limits the collector uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    open_files: u64,
    address_space: u64,
}

/// procfs's `Proc.Limits`: the header line skipped, then every line `name  soft  hard [units]`
/// with the name, starting `Max `, ended by a run of two or more spaces (its regular expression
/// `(Max \w+\s??\w*\s?\w*)\s{2,}(\w+)\s+(\w+)`); a line that does not match, or a soft limit that
/// is neither `unlimited` nor a decimal number, fails the read.
fn parse_limits(data: &str) -> Option<Limits> {
    let mut limits = Limits {
        open_files: 0,
        address_space: 0,
    };
    for line in data.lines().skip(1) {
        let (name, rest) = line.split_once("  ")?;
        let name = name.trim();
        if !name.starts_with("Max ") {
            return None;
        }
        let mut values = rest.split_ascii_whitespace();
        let soft = values.next()?;
        let hard = values.next()?;
        let is_word = |text: &str| {
            text.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        };
        if !is_word(soft) || !is_word(hard) {
            return None;
        }
        match name {
            "Max open files" => limits.open_files = parse_limit(soft)?,
            "Max address space" => limits.address_space = parse_limit(soft)?,
            // procfs parses every limit it knows, and a bad value in any fails the read; it
            // skips a name it does not know without parsing the value.
            _ if KNOWN_LIMITS.contains(&name) => {
                parse_limit(soft)?;
            }
            _ => {}
        }
    }
    Some(limits)
}

/// The limits procfs parses, `Max open files` and `Max address space` aside.
const KNOWN_LIMITS: [&str; 14] = [
    "Max cpu time",
    "Max file size",
    "Max data size",
    "Max stack size",
    "Max core file size",
    "Max resident set",
    "Max processes",
    "Max locked memory",
    "Max file locks",
    "Max pending signals",
    "Max msgqueue size",
    "Max nice priority",
    "Max realtime priority",
    "Max realtime timeout",
];

fn parse_limit(text: &str) -> Option<u64> {
    if text == "unlimited" {
        return Some(UNLIMITED);
    }
    parse_go_uint(text)
}

/// procfs's `parseProcNetstat`: lines in pairs, names then values, split on single spaces, the
/// first field the protocol with its colon; a pair whose field counts differ, or a value that is
/// not a float, fails the read. Returns `IpExt`'s `InOctets` and `OutOctets`, if present.
fn parse_netstat(data: &str) -> Option<(Option<f64>, Option<f64>)> {
    let (mut in_octets, mut out_octets) = (None, None);
    let mut lines = data.lines();
    while let Some(names) = lines.next() {
        // bufio.Scanner yields an empty line once the input is exhausted.
        let values = lines.next().unwrap_or("");
        let names: Vec<&str> = names.split(' ').collect();
        let values: Vec<&str> = values.split(' ').collect();
        if names.len() != values.len() {
            return None;
        }
        let protocol = names[0].strip_suffix(':').unwrap_or(names[0]);
        for (name, value) in names.iter().zip(&values).skip(1) {
            // The kernel writes decimal integers, which Rust parses as Go's ParseFloat does.
            let value: f64 = value.parse().ok()?;
            if protocol == "IpExt" {
                match *name {
                    "InOctets" => in_octets = Some(value),
                    "OutOctets" => out_octets = Some(value),
                    _ => {}
                }
            }
        }
    }
    Some((in_octets, out_octets))
}

#[cfg(test)]
mod tests;
