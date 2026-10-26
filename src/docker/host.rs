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

//! `DOCKER_HOST`, as moby's `ParseHostURL` and go-connections' `ConfigureTransport` read it.

use crate::go_compat::quote::quote;

/// The longest unix socket path: `len(syscall.RawSockaddrUnix{}.Path)` on Linux.
const MAX_UNIX_SOCKET_PATH: usize = 108;

/// Where requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// A unix socket path.
    Unix(String),
    /// A TCP `host:port` address as given (any protocol but `unix` and `npipe` dials TCP).
    Tcp(String),
}

/// A parsed `DOCKER_HOST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Host {
    /// The protocol, before `://`.
    pub proto: String,
    /// The address: the socket path, or the TCP authority without user information.
    pub addr: String,
    /// The path of a `tcp://` URL, which prefixes every request path.
    pub base_path: String,
}

impl Host {
    /// moby's `ParseHostURL`.
    pub(crate) fn parse(host: &str) -> Result<Self, String> {
        let Some((proto, addr)) = host.split_once("://").filter(|(_, addr)| !addr.is_empty())
        else {
            return Err(format!("unable to parse docker host `{host}`"));
        };
        if proto != "tcp" {
            return Ok(Self {
                proto: proto.to_owned(),
                addr: addr.to_owned(),
                base_path: String::new(),
            });
        }
        let (authority, path) = parse_tcp_url(addr)
            .map_err(|reason| format!("parse {}: {reason}", quote(&format!("tcp://{addr}"))))?;
        Ok(Self {
            proto: proto.to_owned(),
            addr: authority,
            base_path: path,
        })
    }

    /// go-connections' `ConfigureTransport`: unix sockets (refusing a path longer than the
    /// kernel takes), no named pipes on Linux, and TCP for every other protocol.
    pub(crate) fn endpoint(&self) -> Result<Endpoint, String> {
        match self.proto.as_str() {
            "unix" => {
                if self.addr.len() > MAX_UNIX_SOCKET_PATH {
                    return Err(format!(
                        "unix socket path {} is too long",
                        quote(&self.addr)
                    ));
                }
                Ok(Endpoint::Unix(self.addr.clone()))
            }
            "npipe" => Err("protocol not available".to_owned()),
            _ => Ok(Endpoint::Tcp(self.addr.clone())),
        }
    }
}

/// The authority (without user information) and the unescaped path of `tcp://<addr>`, as Go's
/// `url.Parse` gives `Host` and `Path`, or its error text after `parse "<url>": `.
fn parse_tcp_url(addr: &str) -> Result<(String, String), String> {
    let without_fragment = addr.split_once('#').map_or(addr, |(before, _)| before);
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(before, _)| before);
    let (authority, path) = without_query
        .find('/')
        .map_or((without_query, ""), |slash| without_query.split_at(slash));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    Ok((parse_host(host)?, unescape_path(path)?))
}

/// Go 1.26's `parseHost`: the host with its `%XX` escapes decoded, or the error after
/// `parse "<url>": `. A bracketed literal must be an IPv6 address (`netip.ParseAddr`), its zone
/// introduced by `%25` (RFC 6874) and decoded too, so `[::1%25lo]` becomes `[::1%lo]`; a port
/// is digits only.
fn parse_host(host: &str) -> Result<String, String> {
    if host.rfind('[').is_some_and(|open| open > 0) {
        return Err("invalid IP-literal".to_owned());
    }
    if host.starts_with('[') {
        let Some(close) = host.rfind(']') else {
            return Err("missing ']' in host".to_owned());
        };
        check_port(&host[close + 1..])?;
        let literal = if let Some(zone) = host[..close].find("%25") {
            let address = unescape_host(&host[..zone], false)?;
            let zone_part = unescape_host(&host[zone..close], true)?;
            format!("{address}{zone_part}")
        } else {
            unescape_host(&host[..close], false)?
        };
        check_ip_literal(&literal[1..])?;
        let rest = unescape_host(&host[close..], false)?;
        return Ok(format!("{literal}{rest}"));
    }
    if let Some(colon) = host.rfind(':') {
        check_port(&host[colon..])?;
    }
    unescape_host(host, false)
}

/// Go's `validOptionalPort`, with its error.
fn check_port(port: &str) -> Result<(), String> {
    let valid = port.is_empty()
        || (port.starts_with(':') && port[1..].bytes().all(|byte| byte.is_ascii_digit()));
    if valid {
        Ok(())
    } else {
        Err(format!("invalid port {} after host", quote(port)))
    }
}

/// `netip.ParseAddr` on a bracketed literal, which must be IPv6 (an IPv4-mapped one included),
/// with a non-empty zone if it has one. The message is Go's for the common mistakes; any other
/// malformed address gets `ParseAddr`'s generic one (Q9).
fn check_ip_literal(literal: &str) -> Result<(), String> {
    let failed = |reason: &str| {
        Err(format!(
            "invalid host: ParseAddr({}): {reason}",
            quote(literal)
        ))
    };
    let (address, zone) = literal
        .split_once('%')
        .map_or((literal, None), |(address, zone)| (address, Some(zone)));
    if address.parse::<std::net::Ipv6Addr>().is_ok() {
        return if zone == Some("") {
            failed("zone must be a non-empty string")
        } else {
            Ok(())
        };
    }
    if zone.is_none() && address.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("invalid IP-literal".to_owned());
    }
    if zone.is_some() && !address.contains(':') {
        return failed("missing IPv6 address");
    }
    failed("unable to parse IP")
}

/// Go's `unescape` for a host (`encodeHost`) or a zone (`encodeZone`): besides `%25`, a `%XX`
/// escape may only stand for a non-ASCII byte in a host, and only for an ASCII byte a host may
/// hold, or a space, in a zone; a byte a host cannot hold is refused in either.
fn unescape_host(host: &str, zone: bool) -> Result<String, String> {
    let bytes = host.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let escape = host.get(index..index + 3).unwrap_or(&host[index..]);
            let value = escape
                .get(1..3)
                .filter(|hex| hex.bytes().all(|digit| digit.is_ascii_hexdigit()))
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("invalid URL escape {}", quote(escape)))?;
            let allowed = escape == "%25"
                || if zone {
                    value == b' '
                        || (value.is_ascii() && value != b'%' && is_host_char(char::from(value)))
                } else {
                    !value.is_ascii()
                };
            if !allowed {
                return Err(format!("invalid URL escape {}", quote(escape)));
            }
            out.push(value);
            index += 3;
        } else {
            if byte.is_ascii() && !is_host_char(char::from(byte)) {
                return Err(format!(
                    "invalid character {} in host name",
                    quote(&char::from(byte).to_string())
                ));
            }
            out.push(byte);
            index += 1;
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The ASCII characters Go's `url` package accepts in a host: unreserved, sub-delims, `:`, `[`,
/// `]`, `%` escapes, and `<`, `>`, `"` kept for compatibility.
fn is_host_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:[]%<>\"".contains(character)
}

/// Go's path unescaping: `%XX` decoded, a bad escape refused.
fn unescape_path(path: &str) -> Result<String, String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let escape = path.get(index..index + 3).unwrap_or(&path[index..]);
            let value = escape
                .get(1..3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("invalid URL escape {}", quote(escape)))?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Go's `path.Join`: the elements joined with `/` and cleaned.
pub(crate) fn join_path(elements: &[&str]) -> String {
    let joined = elements
        .iter()
        .filter(|element| !element.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        return String::new();
    }
    clean_path(&joined)
}

/// Go's `path.Clean`.
fn clean_path(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            _ => parts.push(part),
        }
    }
    let body = parts.join("/");
    match (rooted, body.is_empty()) {
        (true, _) => format!("/{body}"),
        (false, true) => ".".to_owned(),
        (false, false) => body,
    }
}

/// Go's `url.URL.EscapedPath` for a path without a raw form: bytes outside the unreserved set and
/// `$&+,/:;=@` are percent-encoded.
pub(crate) fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+,/:;=@".contains(&byte) {
            out.push(char::from(byte));
        } else {
            push_percent(&mut out, byte);
        }
    }
    out
}

/// Go's `url.URL.String` host escaping: a byte that cannot appear in a host, such as the `/` of
/// a socket path, is percent-encoded (`http://%2Fvar%2Frun%2Fdocker.sock/…`).
pub(crate) fn escape_host(host: &str) -> String {
    let mut out = String::with_capacity(host.len());
    for byte in host.bytes() {
        if byte.is_ascii() && is_host_char(char::from(byte)) && byte != b'%' {
            out.push(char::from(byte));
        } else {
            push_percent(&mut out, byte);
        }
    }
    out
}

/// `%XX`, upper-case as Go writes it.
pub(crate) fn push_percent(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

/// net/http's `removeZone`: the `Host` header of a scoped IPv6 address goes without its zone
/// (`[::1%lo]:2375` is sent as `[::1]:2375`), since the zone means nothing to the server.
pub(crate) fn remove_zone(host: &str) -> String {
    if !host.starts_with('[') {
        return host.to_owned();
    }
    let Some(close) = host.rfind(']') else {
        return host.to_owned();
    };
    match host[..close].rfind('%') {
        Some(zone) => format!("{}{}", &host[..zone], &host[close..]),
        None => host.to_owned(),
    }
}

/// The host and port to dial for a TCP address, as net/http's `canonicalAddr` completes it: the
/// scheme's port when none is given.
pub(crate) fn dial_address(addr: &str, tls: bool) -> (String, u16) {
    let default_port = if tls { 443 } else { 80 };
    if let Some(literal) = addr.strip_prefix('[')
        && let Some((host, rest)) = literal.split_once(']')
    {
        let port = rest
            .strip_prefix(':')
            .and_then(|port| port.parse().ok())
            .unwrap_or(default_port);
        return (host.to_owned(), port);
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            (host.to_owned(), port.parse().unwrap_or(default_port))
        }
        _ => (addr.to_owned(), default_port),
    }
}

#[cfg(test)]
mod tests;
