// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Who is the client behind a reverse proxy: the trusted-proxy list (`PRINTAPI_TRUSTED_PROXIES`),
//! the `X-Forwarded-For` walk and the key the signup throttle uses. Pure: no IO.
//!
//! The walk goes right to left and returns the first address that is not itself a trusted proxy.
//! Caddy replaces an untrusted client's `X-Forwarded-*` with its own; Cloudflare and HAProxy
//! append on the right. Rightmost-untrusted is right for both and never lets a client pick its own
//! address.
//! ponytail: only `X-Forwarded-For` (Caddy's default); `Forwarded` (RFC 7239) is not read.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is the IPv4 address; everything else stays.
fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

/// One entry of the trusted list: an address and a prefix length, host bits already cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Net {
    addr: IpAddr,
    prefix: u8,
}

fn mask_v4(a: Ipv4Addr, prefix: u8) -> u32 {
    let bits = u32::from(a);
    if prefix == 0 {
        0
    } else {
        bits & (u32::MAX << (32 - u32::from(prefix)))
    }
}

fn mask_v6(a: Ipv6Addr, prefix: u8) -> u128 {
    let bits = u128::from(a);
    if prefix == 0 {
        0
    } else {
        bits & (u128::MAX << (128 - u32::from(prefix)))
    }
}

impl Net {
    fn parse(entry: &str) -> Result<Net, String> {
        let bad = || format!("invalid trusted proxy {entry:?}");
        let (addr_s, prefix_s) = match entry.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (entry, None),
        };
        let raw: IpAddr = addr_s.trim().parse().map_err(|_| bad())?;
        let addr = unmap(raw);
        let mapped = matches!((raw, addr), (IpAddr::V6(_), IpAddr::V4(_)));
        let max: u8 = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix_s {
            None => max,
            Some(p) => {
                let n: u8 = p.trim().parse().map_err(|_| bad())?;
                // `::ffff:10.0.0.0/104` is a /8 of IPv4; a shorter prefix would cover IPv6 space
                // outside the mapped range, which has no IPv4 meaning.
                let n = if mapped {
                    n.checked_sub(96).ok_or_else(bad)?
                } else {
                    n
                };
                if n > max {
                    return Err(bad());
                }
                n
            }
        };
        let addr = match addr {
            IpAddr::V4(a) => IpAddr::V4(Ipv4Addr::from(mask_v4(a, prefix))),
            IpAddr::V6(a) => IpAddr::V6(Ipv6Addr::from(mask_v6(a, prefix))),
        };
        Ok(Net { addr, prefix })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, unmap(ip)) {
            (IpAddr::V4(n), IpAddr::V4(a)) => mask_v4(a, self.prefix) == u32::from(n),
            (IpAddr::V6(n), IpAddr::V6(a)) => mask_v6(a, self.prefix) == u128::from(n),
            _ => false,
        }
    }
}

/// The proxies whose `X-Forwarded-For` is believed. Empty (the default) trusts nobody.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<Net>);

impl TrustedProxies {
    /// Comma-separated IPs or CIDRs, whitespace around entries ignored, empty entries skipped.
    /// Any invalid entry is an error (a typo must not silently trust nobody or everybody).
    pub fn parse(s: &str) -> Result<TrustedProxies, String> {
        s.split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(Net::parse)
            .collect::<Result<Vec<_>, _>>()
            .map(TrustedProxies)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(ip))
    }

    /// The entries as text, for display: an address, with `/prefix` unless it names one host.
    pub fn entries(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|n| {
                let max = if n.addr.is_ipv4() { 32 } else { 128 };
                if n.prefix == max {
                    n.addr.to_string()
                } else {
                    format!("{}/{}", n.addr, n.prefix)
                }
            })
            .collect()
    }
}

/// One `X-Forwarded-For` entry: a plain IPv4/IPv6 address or `[IPv6]`.
fn parse_entry(e: &str) -> Option<IpAddr> {
    let e = e.trim();
    let e = e
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(e);
    e.parse().ok().map(unmap)
}

/// The client's address: the peer itself unless the peer is a trusted proxy, then the rightmost
/// `X-Forwarded-For` entry that is not a trusted proxy. An unparseable entry stops the walk and
/// answers the peer: what lies left of garbage is never believed.
pub fn client_ip(peer: IpAddr, xff: &[&str], trusted: &TrustedProxies) -> IpAddr {
    let peer = unmap(peer);
    if !trusted.contains(peer) {
        return peer;
    }
    for e in xff.iter().flat_map(|v| v.split(',')).rev() {
        match parse_entry(e) {
            None => return peer,
            Some(ip) if trusted.contains(ip) => {}
            Some(ip) => return ip,
        }
    }
    peer
}

/// The signup throttle's key for an address: IPv4 per address, IPv6 per /64.
/// ponytail: one client with a /64 would otherwise own 2^64 slots. A /48 holder still gets 65,536.
pub fn throttle_key(ip: IpAddr) -> String {
    match unmap(ip) {
        IpAddr::V4(a) => a.to_string(),
        IpAddr::V6(a) => format!("{}/64", Ipv6Addr::from(mask_v6(a, 64))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }
    fn tp(s: &str) -> TrustedProxies {
        TrustedProxies::parse(s).unwrap()
    }

    #[test]
    fn an_untrusted_peer_cannot_spoof_a_header() {
        let t = tp("127.0.0.1");
        assert_eq!(client_ip(ip("9.9.9.9"), &["1.2.3.4"], &t), ip("9.9.9.9"));
        assert_eq!(
            client_ip(ip("9.9.9.9"), &["1.2.3.4"], &TrustedProxies::default()),
            ip("9.9.9.9")
        );
    }

    #[test]
    fn a_trusted_peer_gives_the_rightmost_untrusted_entry() {
        let t = tp("127.0.0.1");
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["6.6.6.6, 1.2.3.4"], &t),
            ip("1.2.3.4")
        );
        assert_eq!(client_ip(ip("127.0.0.1"), &[], &t), ip("127.0.0.1"));
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["127.0.0.1"], &t),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn several_header_lines_count_as_one_list_in_order() {
        let t = tp("127.0.0.1");
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["6.6.6.6", "5.5.5.5, 1.2.3.4"], &t),
            ip("1.2.3.4")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["5.5.5.5, 127.0.0.1", "127.0.0.1"], &t),
            ip("5.5.5.5")
        );
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_skipped() {
        let t = tp("127.0.0.1, 10.0.0.0/8");
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["1.2.3.4, 10.1.1.1, 10.2.2.2"], &t),
            ip("1.2.3.4")
        );
    }

    #[test]
    fn garbage_stops_the_walk_and_answers_the_peer() {
        let t = tp("127.0.0.1");
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["1.2.3.4, nonsense"], &t),
            ip("127.0.0.1")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["1.2.3.4, nonsense, 5.5.5.5"], &t),
            ip("5.5.5.5")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["1.2.3.4,"], &t),
            ip("127.0.0.1")
        );
        assert_eq!(
            client_ip(ip("127.0.0.1"), &["1.2.3.4:5000"], &t),
            ip("127.0.0.1")
        );
    }

    #[test]
    fn bracketed_and_plain_ipv6_entries() {
        let t = tp("::1");
        assert_eq!(
            client_ip(ip("::1"), &["[2001:db8::7]"], &t),
            ip("2001:db8::7")
        );
        assert_eq!(
            client_ip(ip("::1"), &["2001:db8::7"], &t),
            ip("2001:db8::7")
        );
        assert_eq!(client_ip(ip("::1"), &["[::1]"], &t), ip("::1"));
    }

    #[test]
    fn ipv4_mapped_addresses_are_ipv4() {
        let t = tp("127.0.0.1");
        // the peer arrives mapped (dual-stack listener), the trusted list says plain IPv4
        assert_eq!(
            client_ip(ip("::ffff:127.0.0.1"), &["::ffff:1.2.3.4"], &t),
            ip("1.2.3.4")
        );
        // and the other way round
        let t = tp("::ffff:10.0.0.1");
        assert_eq!(client_ip(ip("10.0.0.1"), &["1.2.3.4"], &t), ip("1.2.3.4"));
        assert_eq!(
            client_ip(ip("::ffff:9.9.9.9"), &["1.2.3.4"], &t),
            ip("9.9.9.9")
        );
    }

    #[test]
    fn cidr_edges() {
        assert!(tp("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(!tp("0.0.0.0/0").contains(ip("2001:db8::1")));
        assert!(tp("::/0").contains(ip("2001:db8::1")));
        assert!(tp("1.2.3.4/32").contains(ip("1.2.3.4")));
        assert!(!tp("1.2.3.4/32").contains(ip("1.2.3.5")));
        assert!(tp("::1/128").contains(ip("::1")));
        assert!(!tp("::1/128").contains(ip("::2")));
        // host bits in the network part are ignored
        assert!(tp("10.1.2.3/8").contains(ip("10.200.0.1")));
        assert!(tp("fd00::1234/16").contains(ip("fd00:ffff::1")));
        assert!(!tp("172.16.0.0/12").contains(ip("172.32.0.1")));
        assert!(tp("172.16.0.0/12").contains(ip("172.31.255.255")));
    }

    #[test]
    fn empty_and_whitespace_trust_nobody_and_invalid_entries_fail() {
        assert!(tp("").is_empty());
        assert!(tp("  ,  ").is_empty());
        assert_eq!(tp(" 127.0.0.1 , ::1 ").0.len(), 2);
        for bad in [
            "nonsense",
            "1.2.3.4/33",
            "::1/129",
            "1.2.3.4/",
            "1.2.3.4/x",
            "1.2.3.4/-1",
            "127.0.0.1,oops",
            "1.2.3.4/8/8",
            "::ffff:10.0.0.0/64",
        ] {
            assert!(TrustedProxies::parse(bad).is_err(), "{bad:?} must fail");
        }
        assert!(tp("::ffff:10.0.0.0/104").contains(ip("10.9.9.9")));
    }

    #[test]
    fn the_throttle_key_is_the_address_for_ipv4_and_the_slash_64_for_ipv6() {
        assert_eq!(throttle_key(ip("1.2.3.4")), "1.2.3.4");
        assert_eq!(throttle_key(ip("::ffff:1.2.3.4")), "1.2.3.4");
        assert_eq!(
            throttle_key(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")),
            throttle_key(ip("2001:db8:1:2::1"))
        );
        assert_ne!(
            throttle_key(ip("2001:db8:1:2::1")),
            throttle_key(ip("2001:db8:1:3::1"))
        );
        assert_eq!(throttle_key(ip("2001:db8:1:2::1")), "2001:db8:1:2::/64");
    }
}
