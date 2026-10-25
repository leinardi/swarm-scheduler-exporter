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

//! Ports of the Go standard library's formatting and parsing that the exporter's behaviour depends
//! on byte for byte: the logs, the flags and the metrics exposition. Each one is a port rather
//! than a Rust equivalent, because the Rust equivalents differ in the details (float ties,
//! `+00:00` for UTC, Unicode versions, POSIX `TZ` strings, sort stability).
//!
//! - [`float`] ports `strconv.FormatFloat(f, 'g', -1, 64)` and encoding/json's float layout;
//! - [`duration`] ports `time.Duration.String` and `time.ParseDuration`;
//! - [`quote`] ports `strconv.Quote`, with Go's Unicode 15 `IsPrint` and `IsSpace`;
//! - [`time_layout`] ports the `time.RFC3339Nano` and `2006-01-02T15:04:05.000Z07:00` layouts;
//! - [`local_zone`] ports how Go's `time.Local` is resolved from `TZ` and the zoneinfo files;
//! - [`parse_float`] ports `strconv.ParseFloat(s, 32)`, which reads `Accept` q-values;
//! - [`sort`] ports `sort.Sort`, whose order matters when the comparison is not a total order;
//! - [`errno`] gives an `io::Error` Go's `syscall.Errno` text;
//! - [`http_status`] ports `net/http.StatusText`;
//! - [`strings`] ports `strings.EqualFold`, `strings.ToLower` and `strings.TrimSpace`, with Go's
//!   Unicode 15 case tables;
//! - [`net_ip`] ports `net.ParseIP`, `net.ParseCIDR`, `IP.Equal` and `IPNet.Contains`.

pub mod duration;
pub mod errno;
pub mod float;
pub mod http_status;
pub mod local_zone;
pub mod net_ip;
pub mod parse_float;
pub mod quote;
pub mod sort;
pub mod strings;
pub mod time_layout;
mod unicode_case;
mod unicode_tables;
