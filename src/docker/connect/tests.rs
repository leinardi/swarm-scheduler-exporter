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

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use super::scoped_ipv6;

fn scoped(scope: u32) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 2375, 0, scope))
}

#[test]
fn a_zone_becomes_the_interface_index_go_dials() {
    let loopback = std::fs::read_to_string("/sys/class/net/lo/ifindex")
        .ok()
        .and_then(|index| index.trim().parse().ok());
    if let Some(index) = loopback {
        assert_eq!(
            scoped_ipv6("::1%lo", 2375),
            Some(scoped(index)),
            "an interface name"
        );
    }
    assert_eq!(scoped_ipv6("::1%7", 2375), Some(scoped(7)), "a number");
    assert_eq!(
        scoped_ipv6("::1%no-such-interface", 2375),
        Some(scoped(0)),
        "an unknown name"
    );
    assert_eq!(scoped_ipv6("::1%..", 2375), Some(scoped(0)), "not a path");
    assert_eq!(scoped_ipv6("::1", 2375), None, "no zone: the resolver's");
    assert_eq!(scoped_ipv6("localhost%lo", 2375), None, "not a literal");
}
