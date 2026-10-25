---
name: go-to-rust-port
description: >
  Hard-won knowledge for porting, rewriting or migrating a Go project (or a Go package) to Rust:
  semantic traps a straight port silently changes, Go library to Rust crate choices with their
  pitfalls, concurrency and shutdown differences, and tooling, lint and static-packaging traps.
  Use when planning or implementing any part of a Go-to-Rust port, and when reviewing such a port
  for behaviour drift.
---

# Porting Go to Rust

Only knowledge that took a spike, a test, a source read or a failed build to find. Each entry is
**trap → fix → (evidence)**, versioned where the version matters, so it can be re-checked. Not a
Rust tutorial. Seen in: awtrix-controller.

## 1. Approach

- Keep the Go code shipping until one breaking cutover commit; until then the Rust code lands
  beside it and the shipped artifact does not change.
- Port leaf modules first, each together with its Go tests. Give every ported test a doc comment
  naming the Go test it replaces (`/// Go: TestX`), and justify every Go test you do not port.
- Capture Go reference values (timestamps, encoded payloads) with a throwaway Go program outside
  the module, rather than reasoning about what Go "probably" returns. Put the operands in variables:
  Go folds a constant expression exactly (`0.1+0.2` is `0.3`), so a constant tests nothing.
- List every intended behaviour change up front. Any other difference from Go is a defect.
- Use non-release commit types until the cutover, so a release-from-commits tool does not ship a
  half-port.

## 2. Semantic traps

### Time

- Go's `time.Date` normalises out-of-range fields (Feb 31 → Mar 3, month 13 → next January), and
  its DST gap/fold resolution is unspecified → decide clamping and gap/fold handling explicitly,
  route every local→instant conversion through one helper, and golden-test both DST days in two
  zones. In practice Go differs by zone (gap: Berlin 02:30 → 03:30, New York 02:30 → 01:30; fold:
  Berlin later, New York earlier), and `AddDate` keeps the shifted wall clock, so a New York daily
  02:30 drifts to 01:30 for good. Capture Go's results before claiming parity. (throwaway Go
  program, go1.25)
- A time of day stored as "duration since local midnight" is an hour off on DST days → store civil
  wall-clock times and resolve them per date. (spike)
- Daily windows resolved to instants can be **empty** on a DST day: a gap pushes the start past
  the end → derive the active state from instants, and make the next-transition search walk
  forward past empty days; a fixed D-1..D+1 horizon stalls until restart. (plan review)
- jiff 0.2's `Disambiguation::Compatible` (gap → shift forward by the gap, fold → earlier) and its
  refusal of `Timestamp + days` are useful guards → keep calendar arithmetic on civil types and
  convert once. (jiff 0.2 docs, spike)
- `time.Duration(n) * time.Hour` wraps on a huge `n`; jiff's `SignedDuration::from_hours` and
  `from_mins` **panic** → build config values with `from_secs(n.saturating_mul(3600))` and
  `saturating_add`. (jiff 0.2 docs; review of a port)
- `Zoned::now()` / `Timestamp::now()` ignore tokio's paused clock → keep a `Clock` trait whose
  test implementation is a base timestamp plus tokio's elapsed time. (spike)
- `time.Parse("15:04", "7:00")` succeeds, so a strict `%H:%M` parser rejects configs Go took;
  `time.Parse("01-02", …)` parses into year 0, a leap year: `02-29` is valid, `02-30` is not.
  (throwaway Go program)
- jiff 0.2's global time zone database prefers the host's zoneinfo even with
  `tzdb-bundle-always`; a trimmed host copy lacks the `backward` aliases (`US/Eastern`,
  `Europe/Kiev`), and jiff matches names case-insensitively where Go does not → look names up in
  `TimeZoneDatabase::bundled()` and require `iana_name() == Some(name)`; special-case `""`, `UTC`
  and `Local` as `time.LoadLocation` does. (probe on a slim host)
- go-solar computes sunrise/sunset for the **UTC** date of the instant → the `sunrise` crate 3
  matches it within ±1 s; decide deliberately whether to use the local date instead. West of UTC
  the UTC date flips in the local evening, so a New York 20:00 in June, before sunset, reads as
  night. (spike; golden test against go-solar v1.0.1)

### Strings

- A Go `string` holds arbitrary bytes, and `utf8.DecodeRune` (and `range`) reads every byte of an
  invalid sequence as its own rune, a truncated `e2 82` included; `&str` cannot hold such bytes,
  and `String::from_utf8_lossy` replaces a whole truncated sequence with **one** U+FFFD → port
  per-rune byte logic (sanitizers, counters) over `&[u8]` with a decoder that takes a complete
  valid sequence whole and steps one byte otherwise (`a\xe2\x82b` → 4 runes in Go, 3 chars lossy).
  (throwaway Go program, go1.26)

### Numbers and text output

- Rust's `{:e}` and `{}` give the shortest round-trip length but break an exact tie between two
  shortest candidates **upward**; Go's `strconv` (and ryu) round to even (`2.1717180002771212e+15`
  in Go, `…213e15` in Rust; 577 of 100,000 random values) → take only the digit count from
  `{:e}` and recompute the digits with `{:.N$e}`, then lay them out as Go's `'g'` (`%e` when the
  exponent is below -4 or at least 6). (250,034 values from Go)
- `strconv.ParseFloat(s, 32)` reads hex floats (`0x1p-1`) and Go-literal underscores (`1_0` is
  10), refuses a sign before `nan`, rounds to `float32`, and on overflow returns ±Inf **with** a
  range error that callers often discard (`q, _ = ParseFloat(...)`); `str::parse::<f32>` rejects
  hex and `_` and accepts `+nan` → port `readFloat` and the hex path, and parse the decimal case
  with Rust once the underscores are checked and stripped. (53 Go inputs, go1.26)
- `sort.Sort` with a comparison that is not a strict weak order (goautoneg's `Accept` sort: q,
  then `*` type, then `*` subtype, each tested alone) has one defined result, Go's pdqsort's;
  Rust's `sort_by` orders it differently and since 1.81 may panic with "user-provided comparison
  function does not correctly implement a total order" → port `sort.Sort` (insertion sort up to
  12 elements, then pdqsort with its seeded `breakPatterns`). (237 Go inputs; a plain insertion
  sort differs on 204)
- `strconv.Quote` and slog's `needsQuoting` use Go's Unicode tables (15.0 in go1.26); Rust 1.98's
  `char` methods and `escape_debug` follow Unicode 17, so code points assigned since print raw in
  Rust and escaped in Go (`\u0897`, `\U0001cc00`) → generate `IsPrint`/`IsSpace` range tables
  from Go and port `strconv.Quote`. (Go 1.26 / Rust 1.98 probe)
- jiff's RFC 3339 output writes UTC as `+00:00` (Go: `Z`) and rounds a sub-minute offset (LMT
  `+00:19:32` → `+00:20`, Go truncates to `+00:19`); `TimeZone::system()` accepts POSIX `TZ`
  strings and case-insensitive names where Go's `time.Local` falls back to UTC → resolve `TZ`
  as Go's `initLocal` does, read the TZif file yourself into `TimeZone::tzif`, and format with
  an in-tree layout. (S7: 70 of 70 layouts; zone probe in `dhi.io/static`)
- Go's `strings.ToLower` maps U+0130 `İ` to a plain `i`; Rust's `to_lowercase` gives `i` plus a
  combining dot, so a case-insensitive match (`İNFO` is a Go log level) differs → special-case
  it. (throwaway Go program)

### JSON and YAML

- `encoding/json` escapes `<`, `>` and `&` as `\u003c`, `\u003e`, `\u0026`; serde_json writes
  them literally → byte golden tests differ there; decide and record it. (spike)
- Go decodes JSON `null` into the zero value silently; serde errors on `null` for `f64` → use
  `Vec<Option<T>>` and decide what a missing value means. (spike)
- `encoding/json` matches keys **case-insensitively** (last duplicate wins), and reads a `null`
  field or a whole-payload `null` as "leave it"; derived serde does none of these, and
  `serde_json::Map` is key-sorted → for input from devices, collect entries in order with a
  `MapAccess` visitor, lowercase the keys, then decode with `#[serde(default)]` and a
  null-as-default helper. (throwaway Go program)
- `json.NewDecoder(r).Decode(&v)` reads the first JSON value and ignores whatever follows;
  `serde_json::from_reader` rejects trailing data → decode with
  `Deserializer::from_reader(r).into_iter::<T>().next()`. (Go docs; review of a port)
- A Go named string type with constants (`type Effect string`) is **open**: any string is sent
  verbatim, so a closed Rust enum narrows it → add an `Other(String)` variant with hand-written
  serde, and keep `omitempty`'s "empty is omitted". (review of a port)
- A Go interface with a custom `MarshalJSON` maps to an enum (untagged or tuple variants);
  embedded structs map to `#[serde(flatten)]`; `omitempty` maps to `skip_serializing_if`, and a
  field Go always emits (even `null`) must not get one. (spike)
- The "zero means unset, apply default" config pattern makes an explicit `0` impossible → use
  `Option<T>` with defaults applied in one place. (spike)
- serde_yaml is archived → serde_yaml_ng 0.10 (errors carry the field path and line), but it
  parses with `unsafe-libyaml` (RUSTSEC-2024-0449, unmaintained): ignore it in cargo-deny with a
  reason, or use serde-saphyr. (spike; cargo-deny)
- yaml.v3 accepts what serde_yaml_ng rejects: `yes`/`no`/`on`/`off`/`y`/`n` bools, a float into an
  int (truncated), `1_000` and `010` (octal) ints, `null` anywhere as the zero value; unknown keys
  are ignored by both → decode a hand-written config through `deserialize_with` visitors that
  mirror yaml.v3 (serde cannot see quoting, so a quoted `"1883"` int slips through). (throwaway Go
  program; serde_yaml_ng probe)

### Logging

- `slog.Level.UnmarshalText` reads a level name in any case plus an optional `+N`/`-N` offset
  (`info+2`), and slog's levels are -4/0/4/8 → port the parser, then map the numeric threshold to
  the tracing filter that passes the same records (`info+1` lets warn through, not info).
  (slog source)
- `slog` prints through its default logger before anything is configured; `tracing` silently
  drops every event until a subscriber exists → install it before anything logs (a config
  loader's warnings); when the log format comes from the config file, peek that key first.
  (seen in awtrix-controller: lost config warnings)
- Go tests capture logs by swapping a handler; the tracing equivalent, a per-test
  `subscriber::set_default`, misses records twice over: it is thread-local, so tasks on a
  multi-thread runtime never see it, and under the parallel test runner tracing caches each
  callsite's interest across all live dispatchers, so a callsite another test registers while
  yours is being installed can cache "never" → assert from a `current_thread` runtime, install a
  global catch-all subscriber once (`set_global_default(tracing_subscriber::registry())`) before
  the scoped one, and call `tracing::callsite::rebuild_interest_cache()` after it. (a test lost
  its warning in 3 of 40 runs, 0 of 40 after)

### CLI

- Go's `flag` accepts `-long` as well as `--long`; clap 4 does not, and worse, reads `-config` as
  the short cluster `-c onfig` when `-c` exists (`-log-level=debug` silently keeps the default),
  reports `-unknown` as `unexpected argument '-u'`, and prints the help and exits 0 for `-hx` →
  before parsing, refuse **every** single-dash argument longer than one character up to `--`,
  naming the `--` spelling for a known flag, and skip the argument after a value flag given
  without `=` (`--label -x` is a value); list it as a breaking change. (spike; seen in
  awtrix-controller; clap 4.6.7)
- Other `flag` semantics clap lacks by default: last repeated value wins
  (`args_override_self`); a value may start with `-` (`allow_hyphen_values`); a bool takes no
  separate value but accepts `=false` and `ParseBool` spellings (`num_args = 0..=1`,
  `require_equals`, `default_missing_value = "true"`, a custom parser); ints are base-0 Go
  literals (`0x10`, `017`, `1_000`); positional arguments are ignored. `-h` exits 0 under
  `flag.ExitOnError`, as clap's does; only a `ContinueOnError` caller picks another code. (flag
  source; port tests; go1.26)
- Go reads `-boolflag false` as **true** plus an ignored positional `false` → a `require_equals`
  bool refuses the `false` as an unexpected argument, which fixes a silent misconfiguration; list
  it as a breaking change. (go1.26 probe)
- clap derive drops a doc comment's trailing period from the help (`Minimum 1s.` → `Minimum
  1s`), and turns the struct's doc comment into the help's about line → `help = "…"` for
  Go-verbatim texts, `about = None, long_about = None`, and `value_name = "BOOL"` with
  `hide_default_value` for bools. (clap 4.6.7 help snapshot)
- No crate parses Go durations (`1h2m3.5s`, `.5s`, `1µs`, `-0`, Go's error texts) → port
  `time.ParseDuration` (about 150 lines, with the `time` package's own byte-wise `quote`, not
  `strconv.Quote`) and table-test it against Go's results. (go1.26: 49 inputs identical)

## 3. Ecosystem map

- **HTTP (net/http):** reqwest's default rustls backend builds `aws-lc-sys` (C), which hurts musl
  cross-builds → ureq 3 with rustls + ring + webpki-roots is lighter (28 vs 86 crates, 3.3 vs
  7.0 MB) and needs no CA files in a static image; run it in `spawn_blocking`. (spike) Its
  `rustls` feature already means ring + webpki-roots, so `default-features = false,
  features = ["rustls"]` only drops gzip; and ureq 3 turns a 4xx/5xx into `Err(StatusCode)`
  unless `http_status_as_error(false)`, which Go's `resp.StatusCode` check needs. (ureq 3.5
  source)
- **Docker (moby client):** bollard 0.21 fails parity: `Url::join` with an absolute path drops
  the `/v1.xx` prefix, so negotiation never reaches the wire; it negotiates through
  `GET /version`, which a socket proxy allowing only the read endpoints answers 403; it sends no
  `Accept` or User-Agent, its timeout covers only the headers, and its closed enums fail a whole
  list on one unknown state → an in-tree client on hyper 1 `client::conn::http1` (pool and
  retries by hand, `title_case_headers(true)`) porting moby's `HEAD /_ping` then `GET /_ping`,
  its discarded negotiation error (`_ = cli.checkVersion`: the request still goes out at the max
  version and the next one pings again), and go-connections' transport: a 10s dial, keep-alive
  15s, two idle connections for 30s, a retry on a reused connection, and on TCP (not unix)
  `Accept-Encoding: gzip` with transparent decompression. go-connections also sets
  `Proxy: http.ProxyFromEnvironment` for TCP hosts; decide whether to keep it. (S1: 8 of 8 ping
  cases against a fake Engine, moby v0.6.0)
- rustls PEM loading needs no rustls-pemfile: `rustls_pki_types::pem::PemObject`
  (`CertificateDer::pem_slice_iter`, `PrivateKeyDer::from_pem_slice`); it skips section kinds it
  does not know (an `ENCRYPTED PRIVATE KEY` is invisible to it), so mirror go-connections' checks
  (a `DEK-Info` header is "encrypted", no certificate in `ca.pem` is an error) on the text. Go's
  `tlsconfig.Client` trusts the system pool **plus** `ca.pem` → `rustls-native-certs` plus the
  file into one `RootCertStore`. (rustls-pki-types 1.15)
- Go reads the Docker event stream as `io.EOF` (clean: only whitespace left and the terminating
  chunk) or anything else, and every real close is a **cut** without the terminating chunk (an
  HAProxy server timeout, a killed proxy, a stopped daemon), so not clean in Go; hyper 1.12
  surfaces a cut as a body `Err` (`unexpected EOF during chunk size line`) with
  `is_incomplete_message() == false` → classify on "the body returned `Err`" vs "the body ended",
  decode with serde_json's `StreamDeserializer` over a growing buffer (an `is_eof()` error means
  "read more"), and port moby's RS (0x1E) filter for `application/json-seq`, which bollard lacks.
  (S1: fake Engine clean, mid-object and cut endings; HAProxy `timeout server 3s`)
- **Logging (slog):** tracing's `Visit` has no duration, time, list or group type (`valuable`
  needs `--cfg tracing_unstable`), `None` fields vanish and `INFO+1` has no `Level`, so a
  slog-compatible format cannot be a tracing formatter → for byte parity, an in-tree logger with
  typed values that ports slog's `commonHandler` step by step. hyper, hyper-util, tokio and rustls
  (no default features) emit nothing through `log`, and hyper-util's `tracing` events vanish
  without a subscriber: no bridge is needed. (tracing-core 0.1.36 `field.rs`; 306 of 306 corpus
  records)
- slog's JSON handler has **two** escapers: strings and keys through its own (`\u0008`, `\u000c`,
  `<>&` literal), a `[]string` through encoding/json (`\b`, `\f`); floats go through
  `json.Marshal` (`%f` unless below 1e-6 or from 1e21, `e-07` → `e-7`; NaN becomes the string
  `"!ERROR:json: unsupported value: NaN"`), text floats through `'g'` → port both escapers and
  three float layouts; serde_json writes `\b` everywhere. (Go 1.26 captures)
- slog's `ReplaceAttr` runs on **every** non-group attribute, so a time stripper keyed `time`
  also drops user attributes named `time`, at any depth; a group it empties is erased without
  restoring the separator (JSON `"msg":"x""k":1`) or the text key prefix (`g.k=1`). (Go 1.26)
- **Prometheus (client_golang, expfmt):** no Rust crate writes Go's text: prometheus 0.14 formats
  floats with `f64::to_string` (`1729771234.123456`, `inf`, `-0`) and sends no `charset` or
  `escaping=`; prometheus-client 0.25.1 is OpenMetrics only, appends `.` to HELP, strips `_total`
  and does not escape HELP or label values; metrics-exporter-prometheus 0.18.3 leaves label pairs
  unsorted and groups families by kind → an in-tree registry and encoder: expfmt's `writeFloat`
  (1, 0 with `-0` as `0`, -1, NaN and ±Inf by hand, else `'g'` with threshold 6, `le` too) and
  `Gather`'s normalisation (label pairs by name, series by label count then values, families by
  name, empty families dropped). (S2 family-set golden, client_golang v1.24.1)
- promhttp's `escaping=` comes from the `Accept` clauses up to the first that matches **any**
  offered format, protobuf included, so a protobuf clause ahead of a text one keeps `underscores`
  → port `NegotiateAccept` with all four Go formats even when only text is written. goautoneg
  reads the first `Accept` header only and `q` untrimmed (`q= 0.5` is 0); gddo's
  `NegotiateContentEncoding` reads every `Accept-Encoding` header, offers identity then gzip with
  a strict `>` (a tie, `gzip, identity`, is identity), compares case-sensitively (`GZIP` is
  identity), turns a best q of 0 into identity, and takes q as `0` or `1` plus decimals (`q=2`
  drops the element, `q=1.5` counts). flate2 (`rust_backend`) writes Go's gzip header bytes but
  other deflate bytes → compare bodies decompressed. (95 Go answers, common v0.71.0)
- client_golang's process collector (procfs v0.21.1) counts CPU and start time in a hard-coded
  100 ticks per second, not `CLK_TCK`; takes `open_fds` from the `st_size` of `/proc/self/fd`
  (Linux 6.2+); reads its network counters from `/proc/self/net/netstat` `IpExt`, which are
  **namespace-wide**, not per process; and without `ReportErrors` silently drops the families a
  failed file feeds. The procfs crate 0.18 has no netstat parser and uses the system tick rate →
  an in-tree `/proc` reader with std only (the page size from `AT_PAGESZ` in `/proc/self/auxv`,
  where Go's runtime gets it). (S6: 9 of 9 values identical on a stopped process)
- **YAML (yaml.v3):** serde_yaml_ng, see §2.

## 4. Concurrency and shutdown

- Dropping a tokio runtime waits for its `spawn_blocking` tasks, so one blocked HTTP call holds
  the process open → build the runtime by hand (not `#[tokio::main]`) and end it with
  `shutdown_timeout`; in a test, `shutdown_background()`. (spike; a fetch test went 10 s → 0.1 s)
- A job that re-derives state from the wall clock when its (monotonic) tokio timer fires can
  read just before the boundary if the clock stepped back or is frozen, and then skips the
  transition → evaluate at `max(clock.now(), due)`. (test with a frozen clock)
- Under a paused tokio clock a recurring job keeps auto-advancing time, so a test waiting for
  something that never happens spins forever → bound waits with `tokio::time::timeout`. (weather
  controller tests)
- Go's `net.Listen` sets `SO_REUSEADDR`; a listener built with socket2 does not unless asked
  (`set_reuse_address(true)`), so a quick restart cannot rebind. (server restart test)
- Give shutdown one overall deadline and hand each step only what remains; never a fresh grace
  period per step, never sequential per-session waits. (plan review)

## 5. Tooling, lints and packaging

- **Lints:** golangci-lint maps to clippy: exhaustive → `wildcard_enum_match_arm`, varnamelen →
  `min_ident_chars`, forbidigo → `print_stdout`/`disallowed_methods` (a `time.Now` ban becomes
  `disallowed-methods` on `Timestamp::now`, `Zoned::now`, `SystemTime::now`, `Instant::now`),
  `//nolint` → `#[expect(lint, reason = "...")]` with `allow_attributes` and
  `allow_attributes_without_reason` denied. `[lints]` is not inherited by workspace members →
  `[workspace.lints]` plus `[lints] workspace = true` in every member (test that it is there),
  and clippy with `--workspace`. (spike)
- clippy's `allow-expect-in-tests` / `allow-unwrap-in-tests` cover `#[test]` functions and
  `#[cfg(test)]` modules, **not** helper functions in `tests/*.rs` → use
  `unwrap_or_else(|error| panic!(...))` there, or a file-level `#![expect]` with a reason.
  (clippy 1.98: failed build)
- cargo-deny 0.20: webpki-roots is `CDLA-Permissive-2.0`, outside a typical MIT/Apache/BSD
  allowlist → allow it with a comment; and an `ignore` entry `{ crate = "name" }` only ignores
  **yanked** versions → ignore advisories by ID (`{ id = "RUSTSEC-…", reason = "…" }`).
  (cargo-deny warnings)
- **Static builds:** cargo-zigbuild produces static musl binaries for amd64 and arm64 from one
  build platform; its stock image can lag the MSRV, so install it on a pinned toolchain. Alpine's
  distro rust links dynamically, and `+crt-static` breaks proc-macros unless `--target` is
  passed. (spike: failing proc-macro build)
- `dhi.io/rust` has no rustup. Its `-alpine*-dev` variant is Alpine's rustc and cannot take
  another target; the `-debian*-dev` one is upstream's standalone install → add a target by
  unpacking `rust-std-<exact version>-<triple>.tar.xz` from static.rust-lang.org and running its
  `install.sh --prefix=/usr/local` (apt-get curl and xz-utils first), and bump the image patch,
  `rust-std` version and digests together by hand. (amd64 and arm64 image builds)
- Go's `t.Skip` has no libtest equivalent: a test gated on an external tool returns early with a
  note on stderr → add an environment variable that turns the missing tool into a panic and set
  it in CI, or the check passes silently exactly where it is meant to guard. (ported meta-tests)
- **Calibration** (one small network service): Go 12 MB binary and 18 MB RSS; Rust 4.9–6.3 MB
  static binaries and about 5 MB RSS. Clean release builds take 36–110 s. (spike)

Verified against (2026-10): Rust 1.98.0, tokio 1.54, jiff 0.2.38, serde_json 1.0, serde_yaml_ng
0.10.0, ureq 3.5.0, sunrise 3.0.0, tracing-subscriber 0.3.23, clap 4.6.7, flate2 1.1.10,
hyper 1.12.0, rustls 0.23.46, cargo-deny 0.20.2, cargo-zigbuild 0.24.0, zig 0.17.0; Go
baselines from go1.25.
