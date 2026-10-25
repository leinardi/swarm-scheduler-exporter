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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{
    Limits, ProcessCollector, Stat, UNLIMITED, auxv_page_size, page_size, parse_boot_time,
    parse_limits, parse_netstat, parse_stat,
};
use jiff::{SignedDuration, Timestamp};

use crate::clock::{Clock as _, SystemClock};
use crate::metrics::testing::gather_series;
use crate::metrics::{Collector, Registry};

/// A `/proc`-like tree in a fresh temporary directory, removed on drop.
struct FakeProc {
    root: PathBuf,
}

impl FakeProc {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "sse-fake-proc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(root.join("self/fd")).unwrap();
        fs::create_dir_all(root.join("self/net")).unwrap();
        Self { root }
    }

    fn write(&self, path: &str, contents: &str) -> &Self {
        fs::write(self.root.join(path), contents).unwrap();
        self
    }

    fn collector(&self) -> Arc<ProcessCollector> {
        Arc::new(ProcessCollector::with_root(&self.root, false))
    }
}

impl Drop for FakeProc {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A `/proc/<pid>/stat` line: the command name holds a space and both parentheses, which only
/// the last `)` ends; utime 150, stime 50, starttime 12345, vsize 19132416, rss 2018.
const STAT: &str = "4242 (a) (b c) S 1 4242 4242 0 -1 4194560 1234 0 0 0 150 50 0 0 20 0 7 0 \
                    12345 19132416 2018 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 \
                    0 0 0 0 0 0 0 0 0\n";

const PROC_STAT: &str = "cpu  1 2 3 4 5 6 7 8 9 10\nintr 1 2\nctxt 3\nbtime 1792918000\n\
                         processes 4\n";

/// A kernel 6.x `/proc/<pid>/limits`, without the trailing spaces the kernel pads each line with
/// (the whitespace hooks strip them here; `parse_limits_follows_procfs` puts them back).
const LIMITS: &str = "\
Limit                     Soft Limit           Hard Limit           Units
Max cpu time              unlimited            unlimited            seconds
Max file size             unlimited            unlimited            bytes
Max data size             unlimited            unlimited            bytes
Max stack size            8388608              unlimited            bytes
Max core file size        0                    unlimited            bytes
Max resident set          unlimited            unlimited            bytes
Max processes             127343               127343               processes
Max open files            524288               524288               files
Max locked memory         8388608              8388608              bytes
Max address space         unlimited            unlimited            bytes
Max file locks            unlimited            unlimited            locks
Max pending signals       127343               127343               signals
Max msgqueue size         819200               819200               bytes
Max nice priority         0                    0
Max realtime priority     0                    0
Max realtime timeout      unlimited            unlimited            us
";

const NETSTAT: &str = "TcpExt: SyncookiesSent SyncookiesRecv\nTcpExt: 0 0\n\
                       IpExt: InNoRoutes InOctets OutOctets\nIpExt: 0 808485686 707916117\n";

fn full_fake_proc() -> FakeProc {
    let fake = FakeProc::new();
    fake.write("self/stat", STAT)
        .write("stat", PROC_STAT)
        .write("self/limits", LIMITS)
        .write("self/net/netstat", NETSTAT);
    for fd in ["0", "1", "2", "7"] {
        fake.write(&format!("self/fd/{fd}"), "");
    }
    fake
}

/// procfs's parse of a stat line, and its failures: no parenthesis, too few fields, a field
/// that is not its Go type (a negative `uint`).
#[test]
fn parse_stat_follows_procfs() {
    assert_eq!(
        parse_stat(STAT),
        Some(Stat {
            utime: 150,
            stime: 50,
            start_ticks: 12345,
            vsize: 19_132_416,
            rss: 2018,
        })
    );
    assert_eq!(parse_stat("4242 a S 1"), None);
    assert_eq!(parse_stat("4242 (a) S 1 2 3"), None);
    assert_eq!(parse_stat(&STAT.replace(" 150 50 ", " -150 50 ")), None);
    // A negative int field is fine (tty_nr is -1 above, and so may rss be).
    assert_eq!(
        parse_stat(&STAT.replace(" 2018 ", " -1 ")).map(|stat| stat.rss),
        Some(-1)
    );
}

#[test]
fn parse_boot_time_follows_procfs() {
    assert_eq!(parse_boot_time(PROC_STAT), Some(1_792_918_000));
    assert_eq!(parse_boot_time("cpu 1 2\n"), Some(0));
    assert_eq!(parse_boot_time("btime x\n"), None);
    assert_eq!(parse_boot_time("btime -1\n"), None);
    // A line with fewer than two fields is skipped.
    assert_eq!(parse_boot_time("btime\nbtime 7\n"), Some(7));
}

#[test]
fn parse_limits_follows_procfs() {
    let padded: String = LIMITS.lines().fold(String::new(), |mut padded, line| {
        padded.push_str(line);
        padded.push_str("     \n");
        padded
    });
    for limits in [LIMITS, padded.as_str()] {
        assert_eq!(
            parse_limits(limits),
            Some(Limits {
                open_files: 524_288,
                address_space: UNLIMITED,
            })
        );
    }
    // A line that is not a limit, or a bad value of a known limit, fails the read; an unknown
    // limit is skipped unparsed.
    assert_eq!(parse_limits(&format!("{LIMITS}garbage\n")), None);
    assert_eq!(
        parse_limits(&LIMITS.replace(
            "8388608              unlimited",
            "8x8                  unlimited"
        )),
        None
    );
    assert_eq!(
        parse_limits(
            "header\nMax new thing             x-y                  1                    z\n"
        ),
        None
    );
    assert_eq!(
        parse_limits(
            "header\nMax new thing             huge                 1                    z\n"
        ),
        Some(Limits {
            open_files: 0,
            address_space: 0,
        })
    );
}

#[test]
fn parse_netstat_follows_procfs() {
    assert_eq!(
        parse_netstat(NETSTAT),
        Some((Some(808_485_686.0), Some(707_916_117.0)))
    );
    // No IpExt line: both absent, which the collector writes as 0.
    assert_eq!(parse_netstat("TcpExt: A\nTcpExt: 1\n"), Some((None, None)));
    // Field counts that differ, a missing values line, or a value that is not a number fail.
    assert_eq!(parse_netstat("IpExt: InOctets OutOctets\nIpExt: 1\n"), None);
    assert_eq!(parse_netstat("IpExt: InOctets\n"), None);
    assert_eq!(parse_netstat("IpExt: InOctets\nIpExt: x\n"), None);
}

/// `AT_PAGESZ` is found among the auxv pairs, and the vector ends at `AT_NULL`.
#[test]
fn auxv_page_size_reads_at_pagesz() {
    let words = |pairs: &[(usize, usize)]| -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|&(key, value)| key.to_ne_bytes().into_iter().chain(value.to_ne_bytes()))
            .collect()
    };
    assert_eq!(
        auxv_page_size(&words(&[(33, 1), (6, 16384), (0, 0)])),
        Some(16384)
    );
    assert_eq!(auxv_page_size(&words(&[(33, 1), (0, 0), (6, 4096)])), None);
    assert_eq!(auxv_page_size(&[]), None);
    assert!(page_size().is_some_and(|size| size >= 4096));
}

/// Every family from a full tree, with procfs's arithmetic: ticks over 100, RSS in pages, the
/// start time from btime, and the entries of a fd directory that is not the real /proc.
#[test]
fn collects_every_family_from_a_proc_tree() {
    let fake = full_fake_proc();
    let rss = 2018.0 * f64::from(u32::try_from(page_size().unwrap()).unwrap());
    let want = [
        ("process_cpu_seconds_total{}", 2.0),
        ("process_max_fds{}", 524_288.0),
        ("process_network_receive_bytes_total{}", 808_485_686.0),
        ("process_network_transmit_bytes_total{}", 707_916_117.0),
        ("process_open_fds{}", 4.0),
        ("process_resident_memory_bytes{}", rss),
        ("process_start_time_seconds{}", 1_792_918_123.45),
        ("process_virtual_memory_bytes{}", 19_132_416.0),
        (
            "process_virtual_memory_max_bytes{}",
            1.844_674_407_370_955_2e19,
        ),
    ]
    .map(|(name, value)| (name.to_owned(), value))
    .into();
    assert_eq!(gather_series(fake.collector()), want);
}

/// A file that cannot be read or parsed drops only the families it feeds, as client_golang
/// does without `ReportErrors`.
#[test]
fn a_failed_read_drops_only_its_families() {
    let names = |fake: &FakeProc| -> Vec<String> {
        gather_series(fake.collector())
            .into_keys()
            .map(|id| id.trim_end_matches("{}").to_owned())
            .collect()
    };
    let without = |dropped: &[&str]| -> Vec<String> {
        [
            "process_cpu_seconds_total",
            "process_max_fds",
            "process_network_receive_bytes_total",
            "process_network_transmit_bytes_total",
            "process_open_fds",
            "process_resident_memory_bytes",
            "process_start_time_seconds",
            "process_virtual_memory_bytes",
            "process_virtual_memory_max_bytes",
        ]
        .into_iter()
        .filter(|name| !dropped.contains(name))
        .map(str::to_owned)
        .collect()
    };

    let fake = full_fake_proc();
    fake.write("self/stat", "garbage");
    assert_eq!(
        names(&fake),
        without(&[
            "process_cpu_seconds_total",
            "process_resident_memory_bytes",
            "process_start_time_seconds",
            "process_virtual_memory_bytes",
        ])
    );

    let fake = full_fake_proc();
    fake.write("stat", "btime x\n");
    assert_eq!(names(&fake), without(&["process_start_time_seconds"]));

    let fake = full_fake_proc();
    fake.write("self/limits", "header\nnot a limit\n");
    assert_eq!(
        names(&fake),
        without(&["process_max_fds", "process_virtual_memory_max_bytes"])
    );

    let fake = full_fake_proc();
    fake.write("self/net/netstat", "IpExt: InOctets\nIpExt: 1 2\n");
    assert_eq!(
        names(&fake),
        without(&[
            "process_network_receive_bytes_total",
            "process_network_transmit_bytes_total",
        ])
    );

    let fake = full_fake_proc();
    fs::remove_dir_all(fake.root.join("self/fd")).unwrap();
    assert_eq!(names(&fake), without(&["process_open_fds"]));

    // Nothing readable: no family at all.
    let empty = ProcessCollector::with_root(Path::new("/nonexistent-proc"), false);
    let mut families = Vec::new();
    empty.collect(&mut families);
    assert!(families.is_empty());
}

/// This process, read from the real /proc: the values are plausible.
#[test]
fn reads_plausible_values_for_this_process() {
    let series = gather_series(Arc::new(ProcessCollector::new()));
    let value = |name: &str| series[&format!("{name}{{}}")];
    assert_eq!(series.len(), 9, "{series:?}");
    assert!(value("process_resident_memory_bytes") > 0.0);
    assert!(value("process_virtual_memory_bytes") >= value("process_resident_memory_bytes"));
    assert!(value("process_open_fds") >= 3.0);
    assert!(value("process_max_fds") >= value("process_open_fds"));
    assert!(value("process_cpu_seconds_total") >= 0.0);
    let started = Timestamp::from_duration(
        SignedDuration::try_from_secs_f64(value("process_start_time_seconds")).unwrap(),
    )
    .unwrap();
    let age = SystemClock.now().duration_since(started);
    // Within the last minute, give or take btime's whole second.
    assert!(
        age > SignedDuration::from_secs(-1) && age < SignedDuration::from_mins(1),
        "started {started}, {age} ago"
    );
}

/// The registry gathers the collector like any other.
#[test]
fn registers_its_nine_families() {
    let registry = Registry::new();
    registry
        .register(Arc::new(ProcessCollector::new()))
        .unwrap();
    assert_eq!(registry.gather().len(), 9);
}
