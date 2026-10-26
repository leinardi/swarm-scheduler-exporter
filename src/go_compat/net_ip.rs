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

//! Ports of `net.ParseIP`, `net.ParseCIDR`, `IP.Equal` and `IPNet.Contains`, which the
//! `node.ip` placement constraint compares with. Addresses parse as `netip.ParseAddr` does (no
//! zone, no leading zeros in IPv4), which Rust's parser matches on the Go corpus in
//! `tests/testdata/net`; the byte layouts, the prefix length's `dtoi` and the IPv4-in-IPv6
//! handling of the mask and the containment are Go's.

use std::net::IpAddr;

/// The 12-byte prefix of an IPv4 address in IPv6 form.
const V4_IN_V6_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];

/// `netip.ParseAddr` without a zone: the address, and whether it is IPv4.
fn parse_addr(text: &str) -> Option<IpAddr> {
    text.parse().ok()
}

fn as16(addr: IpAddr) -> [u8; 16] {
    match addr {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

/// Go's `net.ParseIP`, always in its 16-byte form; `None` for Go's nil.
#[must_use]
pub fn parse_ip(text: &str) -> Option<[u8; 16]> {
    parse_addr(text).map(as16)
}

/// Go's `IP.Equal` of two parsed (16-byte) addresses, where a nil one equals nothing.
#[must_use]
pub fn ip_equal(left: &[u8; 16], right: Option<&[u8; 16]>) -> bool {
    right.is_some_and(|right| left == right)
}

/// Go's `IP.To4`.
fn to4(ip: &[u8]) -> Option<&[u8]> {
    match ip.len() {
        4 => Some(ip),
        16 if ip[..12] == V4_IN_V6_PREFIX => Some(&ip[12..]),
        _ => None,
    }
}

/// A Go `net.IPNet`: the network number and the mask, each 4 or 16 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpNet {
    ip: Vec<u8>,
    mask: Vec<u8>,
}

impl IpNet {
    /// The network number.
    #[must_use]
    pub fn ip(&self) -> &[u8] {
        &self.ip
    }

    /// The mask.
    #[must_use]
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    /// Go's `networkNumberAndMask`.
    fn number_and_mask(&self) -> Option<(&[u8], &[u8])> {
        let ip = match to4(&self.ip) {
            Some(ip) => ip,
            None if self.ip.len() == 16 => &self.ip,
            None => return None,
        };
        let mask = match self.mask.len() {
            4 if ip.len() == 4 => &self.mask[..],
            16 if ip.len() == 4 => &self.mask[12..],
            16 => &self.mask[..],
            _ => return None,
        };
        Some((ip, mask))
    }

    /// Go's `IPNet.Contains` of a parsed address; a nil one is in no network.
    #[must_use]
    pub fn contains(&self, ip: Option<&[u8; 16]>) -> bool {
        let Some((network, mask)) = self.number_and_mask() else {
            return false;
        };
        let Some(ip) = ip else {
            return false;
        };
        let ip = to4(ip).unwrap_or(ip);
        ip.len() == network.len()
            && network
                .iter()
                .zip(mask)
                .zip(ip)
                .all(|((network, mask), ip)| network & mask == ip & mask)
    }
}

/// Go's `dtoi`: a decimal prefix of `text`, refused past 0xFFFFFF or when there is none.
fn dtoi(text: &str) -> Option<(u32, usize)> {
    const BIG: u32 = 0x00FF_FFFF;
    let mut value: u32 = 0;
    let mut length = 0;
    for byte in text.bytes().take_while(u8::is_ascii_digit) {
        value = value * 10 + u32::from(byte - b'0');
        if value >= BIG {
            return None;
        }
        length += 1;
    }
    (length > 0).then_some((value, length))
}

/// Go's `CIDRMask(ones, bits)` for a valid prefix length.
fn cidr_mask(ones: u32, bits: u32) -> Vec<u8> {
    let mut remaining = ones;
    (0..bits / 8)
        .map(|_| {
            if remaining >= 8 {
                remaining -= 8;
                0xff
            } else {
                let byte = !(0xff_u8 >> remaining);
                remaining = 0;
                byte
            }
        })
        .collect()
}

/// Go's `IP.Mask` of a 16-byte address.
fn mask_ip(ip: &[u8; 16], mask: &[u8]) -> Option<Vec<u8>> {
    let ip: &[u8] = if mask.len() == 4 && ip[..12] == V4_IN_V6_PREFIX {
        &ip[12..]
    } else {
        ip
    };
    (ip.len() == mask.len()).then(|| ip.iter().zip(mask).map(|(ip, mask)| ip & mask).collect())
}

/// Go's `net.ParseCIDR`, the network only; `None` for its error.
#[must_use]
pub fn parse_cidr(text: &str) -> Option<IpNet> {
    let (addr, prefix) = text.split_once('/')?;
    let addr = parse_addr(addr)?;
    let bits = if addr.is_ipv4() { 32 } else { 128 };
    let (ones, length) = dtoi(prefix)?;
    if length != prefix.len() || ones > bits {
        return None;
    }
    let mask = cidr_mask(ones, bits);
    let ip = mask_ip(&as16(addr), &mask)?;
    Some(IpNet { ip, mask })
}
