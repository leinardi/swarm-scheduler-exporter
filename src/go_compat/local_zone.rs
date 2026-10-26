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

//! The local time zone, resolved the way Go's `time` package resolves `time.Local` on Unix
//! (`initLocal`):
//!
//! - `TZ` unset: the `TZif` file `/etc/localtime`;
//! - `TZ` empty, or `UTC`: UTC;
//! - `TZ` naming an absolute path (an optional leading `:` is dropped): that file;
//! - any other `TZ`: the file of that exact name under the first zoneinfo directory that has a
//!   valid one (`/usr/share/zoneinfo/`, `/usr/share/lib/zoneinfo/`, `/usr/lib/locale/TZ/`,
//!   `/etc/zoneinfo`);
//! - anything that fails: UTC.
//!
//! jiff's `TimeZone::system()` is laxer than Go (S7): it parses a POSIX `TZ` string such as
//! `CET-1CEST,M3.5.0,M10.5.0/3` and looks names up case-insensitively, where Go falls back to UTC
//! for both. So the files are read here and handed to `TimeZone::tzif`. Go would also try a
//! `zoneinfo.zip` under its build-time `GOROOT` as a last resort; the shipped image has no Go
//! tree, so that source never answers and is not ported.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;

/// The zoneinfo directories Go's `platformZoneSources` lists, in order.
pub const PLATFORM_ZONE_SOURCES: &[&str] = &[
    "/usr/share/zoneinfo/",
    "/usr/share/lib/zoneinfo/",
    "/usr/lib/locale/TZ/",
    "/etc/zoneinfo",
];

/// The system default zone file Go reads when `TZ` is unset.
pub const LOCALTIME: &str = "/etc/localtime";

/// The local zone, from `getenv("TZ")` and the system's files.
#[must_use]
pub fn local_zone(getenv: impl Fn(&str) -> Option<String>) -> TimeZone {
    let sources: Vec<PathBuf> = PLATFORM_ZONE_SOURCES.iter().map(PathBuf::from).collect();
    resolve(getenv("TZ").as_deref(), Path::new(LOCALTIME), &sources)
}

/// [`local_zone`] over explicit files: the value of `TZ` (`None` when unset), the system default
/// zone file, and the zoneinfo directories.
#[must_use]
pub fn resolve(tz: Option<&str>, localtime: &Path, sources: &[PathBuf]) -> TimeZone {
    let Some(tz) = tz else {
        return load(localtime).unwrap_or(TimeZone::UTC);
    };
    let name = tz.strip_prefix(':').unwrap_or(tz);
    if name.starts_with('/') {
        return load(Path::new(name)).unwrap_or(TimeZone::UTC);
    }
    if name.is_empty() || name == "UTC" {
        return TimeZone::UTC;
    }
    // Go joins source + "/" + name, so a name with ".." or a trailing slash reads whatever file
    // that path names, as here.
    sources
        .iter()
        .find_map(|source| load(&source.join(name)))
        .unwrap_or(TimeZone::UTC)
}

/// The zone in the `TZif` file at `path`, if it reads and parses.
fn load(path: &Path) -> Option<TimeZone> {
    let data = fs::read(path).ok()?;
    TimeZone::tzif(&path.to_string_lossy(), &data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    /// A minimal `TZif` version 1 file: one zone type at `offset` seconds, no transitions.
    fn fixed_tzif(offset: i32) -> Vec<u8> {
        let mut data = b"TZif".to_vec();
        data.push(0); // version 1
        data.extend([0u8; 15]);
        // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        for count in [0u32, 0, 0, 0, 1, 4] {
            data.extend(count.to_be_bytes());
        }
        data.extend(offset.to_be_bytes());
        data.extend([0, 0]); // isdst, abbreviation index
        data.extend(b"XYZ\0");
        data
    }

    /// A scratch filesystem: a zoneinfo directory holding `Europe/Berlin` (+01:00 for the test)
    /// and an `/etc/localtime` at +03:00.
    struct Scratch {
        directory: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let directory =
                std::env::temp_dir().join(format!("sse-local-zone-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&directory);
            fs::create_dir_all(directory.join("zoneinfo/Europe")).unwrap();
            fs::write(directory.join("zoneinfo/Europe/Berlin"), fixed_tzif(3600)).unwrap();
            fs::write(directory.join("zoneinfo/Broken"), b"not a tzif file").unwrap();
            fs::write(directory.join("localtime"), fixed_tzif(3 * 3600)).unwrap();
            Self { directory }
        }

        fn resolve(&self, tz: Option<&str>) -> i32 {
            let zone = resolve(
                tz,
                &self.directory.join("localtime"),
                &[
                    self.directory.join("missing"),
                    self.directory.join("zoneinfo"),
                ],
            );
            zone.to_offset(Timestamp::UNIX_EPOCH).seconds()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn tz_unset_reads_the_system_default() {
        let scratch = Scratch::new("unset");
        assert_eq!(scratch.resolve(None), 3 * 3600);
    }

    #[test]
    fn tz_unset_without_a_system_default_is_utc() {
        let zone = resolve(None, Path::new("/nonexistent/localtime"), &[]);
        assert_eq!(zone.to_offset(Timestamp::UNIX_EPOCH).seconds(), 0);
    }

    #[test]
    fn tz_empty_or_utc_is_utc() {
        let scratch = Scratch::new("empty");
        assert_eq!(scratch.resolve(Some("")), 0);
        assert_eq!(scratch.resolve(Some(":")), 0);
        assert_eq!(scratch.resolve(Some("UTC")), 0);
    }

    #[test]
    fn tz_name_is_looked_up_with_its_exact_case() {
        let scratch = Scratch::new("name");
        assert_eq!(scratch.resolve(Some("Europe/Berlin")), 3600);
        assert_eq!(scratch.resolve(Some(":Europe/Berlin")), 3600);
        // jiff's system lookup would accept this; Go falls back to UTC.
        assert_eq!(scratch.resolve(Some("europe/berlin")), 0);
    }

    #[test]
    fn tz_posix_string_is_utc() {
        let scratch = Scratch::new("posix");
        assert_eq!(scratch.resolve(Some("CET-1CEST,M3.5.0,M10.5.0/3")), 0);
    }

    #[test]
    fn tz_missing_or_invalid_zone_is_utc() {
        let scratch = Scratch::new("missing");
        assert_eq!(scratch.resolve(Some("Mars/Olympus")), 0);
        assert_eq!(scratch.resolve(Some("Broken")), 0);
        let no_zoneinfo = resolve(Some("Europe/Berlin"), Path::new("/nonexistent"), &[]);
        assert_eq!(no_zoneinfo.to_offset(Timestamp::UNIX_EPOCH).seconds(), 0);
    }

    #[test]
    fn tz_absolute_path_reads_that_file() {
        let scratch = Scratch::new("absolute");
        let path = scratch.directory.join("zoneinfo/Europe/Berlin");
        let tz = path.to_string_lossy().into_owned();
        assert_eq!(scratch.resolve(Some(&tz)), 3600);
        assert_eq!(scratch.resolve(Some(&format!(":{tz}"))), 3600);
        assert_eq!(scratch.resolve(Some("/nonexistent/zone")), 0);
    }

    #[test]
    fn local_zone_reads_tz_from_getenv() {
        let zone = local_zone(|name| (name == "TZ").then(String::new));
        assert_eq!(zone.to_offset(Timestamp::UNIX_EPOCH).seconds(), 0);
    }
}
