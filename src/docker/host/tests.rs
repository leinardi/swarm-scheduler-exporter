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

use super::{Endpoint, Host, dial_address, escape_host, escape_path, join_path, remove_zone};

fn host(proto: &str, addr: &str, base_path: &str) -> Host {
    Host {
        proto: proto.to_owned(),
        addr: addr.to_owned(),
        base_path: base_path.to_owned(),
    }
}

/// moby's `ParseHostURL` on the forms `DOCKER_HOST` takes, and its refusals.
#[test]
fn parse_follows_moby() {
    assert_eq!(
        Host::parse("unix:///var/run/docker.sock"),
        Ok(host("unix", "/var/run/docker.sock", ""))
    );
    assert_eq!(
        Host::parse("tcp://127.0.0.1:2375"),
        Ok(host("tcp", "127.0.0.1:2375", ""))
    );
    // The path of a tcp URL prefixes every request; user information, query and fragment go.
    assert_eq!(
        Host::parse("tcp://user@docker.example:2376/base%20path/?x=1#y"),
        Ok(host("tcp", "docker.example:2376", "/base path/"))
    );
    assert_eq!(
        Host::parse("tcp://[::1]:2375"),
        Ok(host("tcp", "[::1]:2375", ""))
    );
    // Any other protocol keeps its address as written.
    assert_eq!(
        Host::parse("ssh://user@host"),
        Ok(host("ssh", "user@host", ""))
    );
    for bad in ["/var/run/docker.sock", "unix://", "tcp:/x", ""] {
        assert_eq!(
            Host::parse(bad),
            Err(format!("unable to parse docker host `{bad}`")),
            "{bad}"
        );
    }
    assert_eq!(
        Host::parse("tcp://host:port"),
        Err("parse \"tcp://host:port\": invalid port \":port\" after host".to_owned())
    );
    assert_eq!(
        Host::parse("tcp://a b:1"),
        Err("parse \"tcp://a b:1\": invalid character \" \" in host name".to_owned())
    );
    assert_eq!(
        Host::parse("tcp://[::1:2375"),
        Err("parse \"tcp://[::1:2375\": missing ']' in host".to_owned())
    );
    assert_eq!(
        Host::parse("tcp://h:1/%zz"),
        Err("parse \"tcp://h:1/%zz\": invalid URL escape \"%zz\"".to_owned())
    );
}

/// Go 1.26's `url.Parse` host rules, each answer captured from it: escapes are decoded (`%25`
/// introduces an IPv6 zone, other host escapes only stand for non-ASCII bytes), and a bracketed
/// literal must be an IPv6 address.
#[test]
fn hosts_are_unescaped_and_checked_as_go_does() {
    for (addr, want) in [
        ("[::1%25lo]:2375", "[::1%lo]:2375"),
        ("[fe80::1%25eth0]:2376", "[fe80::1%eth0]:2376"),
        ("[::1%251]:2375", "[::1%1]:2375"),
        ("[::1%25en%200]:1", "[::1%en 0]:1"),
        ("[::1%25en%41]:1", "[::1%enA]:1"),
        ("[::1%25lo]", "[::1%lo]"),
        ("[::1%25lo]:", "[::1%lo]:"),
        ("[::1%25lo%25x]:1", "[::1%lo%x]:1"),
        ("[::1%25a%20b]:1", "[::1%a b]:1"),
        ("[fe80::1%25eth0.100]:1", "[fe80::1%eth0.100]:1"),
        ("[::ffff:1.2.3.4]:1", "[::ffff:1.2.3.4]:1"),
        ("[::1]", "[::1]"),
        ("h%c3%a9:1", "h\u{e9}:1"),
        ("h%25:1", "h%:1"),
    ] {
        assert_eq!(
            Host::parse(&format!("tcp://{addr}")),
            Ok(host("tcp", want, "")),
            "{addr}"
        );
    }
    for (addr, error) in [
        ("[::1%lo]:2375", "invalid URL escape \"%lo\""),
        ("[::1%25en%2F0]:1", "invalid URL escape \"%2F\""),
        ("[::1%25%C3%A9]:1", "invalid URL escape \"%C3\""),
        ("[::1%25a%7Cb]:1", "invalid URL escape \"%7C\""),
        ("[::1%25a|b]:1", "invalid character \"|\" in host name"),
        ("[%41::1]:1", "invalid URL escape \"%41\""),
        ("h%41st:1", "invalid URL escape \"%41\""),
        ("a%2:1", "invalid URL escape \"%2:\""),
        ("[::1]%25:1", "invalid port \"%25:1\" after host"),
        ("[::1%25lo]x:1", "invalid port \"x:1\" after host"),
        ("[::1]junk", "invalid port \"junk\" after host"),
        ("a[b:1", "invalid IP-literal"),
        ("[1.2.3.4]:1", "invalid IP-literal"),
        (
            "[zz]:1",
            "invalid host: ParseAddr(\"zz\"): unable to parse IP",
        ),
        ("[]:1", "invalid host: ParseAddr(\"\"): unable to parse IP"),
        (
            "[zz%25lo]:1",
            "invalid host: ParseAddr(\"zz%lo\"): missing IPv6 address",
        ),
        (
            "[%25lo]:1",
            "invalid host: ParseAddr(\"%lo\"): missing IPv6 address",
        ),
        (
            "[::1%25]:1",
            "invalid host: ParseAddr(\"::1%\"): zone must be a non-empty string",
        ),
    ] {
        assert_eq!(
            Host::parse(&format!("tcp://{addr}")),
            Err(format!("parse \"tcp://{addr}\": {error}")),
            "{addr}"
        );
    }
    // Go words this one as netip does (`unexpected character, want colon`); the port names any
    // other malformed literal with `ParseAddr`'s generic reason (Q9), and refuses it all the same.
    assert_eq!(
        Host::parse("tcp://[::1]:1]:2"),
        Err(
            "parse \"tcp://[::1]:1]:2\": invalid host: ParseAddr(\"::1]:1\"): unable to parse IP"
                .to_owned()
        )
    );
}

/// net/http's `removeZone`, for the `Host` header.
#[test]
fn the_host_header_goes_without_the_zone() {
    assert_eq!(remove_zone("[::1%lo]:2375"), "[::1]:2375");
    assert_eq!(remove_zone("[fe80::1%eth0]"), "[fe80::1]");
    assert_eq!(remove_zone("[::1]:2375"), "[::1]:2375");
    assert_eq!(remove_zone("h%:1"), "h%:1");
    assert_eq!(remove_zone("[::1%lo:1"), "[::1%lo:1");
}

/// go-connections' `ConfigureTransport`: unix sockets up to 108 bytes, no named pipes, TCP for
/// the rest.
#[test]
fn endpoint_follows_go_connections() {
    assert_eq!(
        host("unix", "/run/d.sock", "").endpoint(),
        Ok(Endpoint::Unix("/run/d.sock".to_owned()))
    );
    let long = format!("/{}", "a".repeat(108));
    assert_eq!(
        host("unix", &long, "").endpoint(),
        Err(format!("unix socket path \"{long}\" is too long"))
    );
    assert!(host("unix", &long[..108], "").endpoint().is_ok());
    assert_eq!(
        host("npipe", "//./pipe/docker_engine", "").endpoint(),
        Err("protocol not available".to_owned())
    );
    for proto in ["tcp", "http", "https", "ssh"] {
        assert_eq!(
            host(proto, "h:1", "").endpoint(),
            Ok(Endpoint::Tcp("h:1".to_owned()))
        );
    }
}

/// Go's `path.Join` and the URL escaping of paths and hosts.
#[test]
fn paths_join_and_escape_as_go() {
    assert_eq!(join_path(&["", "/v1.56", "/nodes"]), "/v1.56/nodes");
    assert_eq!(
        join_path(&["/base/", "/v1.56", "/nodes"]),
        "/base/v1.56/nodes"
    );
    assert_eq!(join_path(&["", "/_ping"]), "/_ping");
    assert_eq!(join_path(&["/a/./b/../c", "/d/"]), "/a/c/d");
    assert_eq!(escape_path("/base path/v1.56"), "/base%20path/v1.56");
    assert_eq!(
        escape_host("/var/run/docker.sock"),
        "%2Fvar%2Frun%2Fdocker.sock"
    );
    assert_eq!(escape_host("127.0.0.1:2375"), "127.0.0.1:2375");
}

/// net/http's `canonicalAddr`: the scheme's port when none is given.
#[test]
fn dial_address_defaults_the_port() {
    assert_eq!(dial_address("h:2375", false), ("h".to_owned(), 2375));
    assert_eq!(dial_address("h", false), ("h".to_owned(), 80));
    assert_eq!(dial_address("h", true), ("h".to_owned(), 443));
    assert_eq!(dial_address("[::1]:2376", true), ("::1".to_owned(), 2376));
    assert_eq!(dial_address("[::1]", true), ("::1".to_owned(), 443));
    assert_eq!(
        dial_address("[::1%lo]:2375", false),
        ("::1%lo".to_owned(), 2375)
    );
}
