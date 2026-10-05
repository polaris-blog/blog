//! Resolve client addresses only through explicitly trusted proxy peers.
use std::net::IpAddr;

use axum::http::HeaderMap;

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

/// Proxies must append the connecting peer to X-Forwarded-For. Walking
/// right-to-left stops at the first untrusted hop, ignoring attacker-supplied
/// entries to its left. Invalid or oversized chains fall back to the peer.
pub fn resolve(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpAddr]) -> IpAddr {
    let peer = normalize(peer);
    let trusted = |ip| trusted_proxies.iter().any(|p| normalize(*p) == ip);
    if !trusted(peer) {
        return peer;
    }
    let mut hops = Vec::new();
    for header in headers.get_all("x-forwarded-for") {
        let Ok(value) = header.to_str() else {
            return peer;
        };
        for part in value.split(',') {
            if hops.len() == 32 {
                return peer;
            }
            let Ok(ip) = part.trim().parse::<IpAddr>() else {
                return peer;
            };
            hops.push(normalize(ip));
        }
    }
    let mut client = peer;
    for hop in hops.into_iter().rev() {
        if !trusted(client) {
            break;
        }
        client = hop;
    }
    client
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_trust_and_spoofing_boundaries() {
        let trusted = ["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()];
        for (peer, forwarded, expected) in [
            ("198.51.100.1", "192.0.2.1", "198.51.100.1"),
            ("127.0.0.1", "198.51.100.1", "198.51.100.1"),
            ("127.0.0.1", "192.0.2.99, 198.51.100.1", "198.51.100.1"),
            ("127.0.0.1", "198.51.100.1, ::1", "198.51.100.1"),
            ("::ffff:127.0.0.1", "::ffff:198.51.100.1", "198.51.100.1"),
            ("::1", "2001:db8::1", "2001:db8::1"),
            ("127.0.0.1", "garbage, 198.51.100.1", "127.0.0.1"),
            ("127.0.0.1", "198.51.100.1,", "127.0.0.1"),
            ("127.0.0.1", "198.51.100.1:3000", "127.0.0.1"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", forwarded.parse().unwrap());
            assert_eq!(
                resolve(peer.parse().unwrap(), &headers, &trusted),
                expected.parse::<IpAddr>().unwrap()
            );
        }
    }

    #[test]
    fn repeated_headers_are_one_chain_and_trust_is_opt_in() {
        let peer = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(resolve(peer, &headers, &[peer]), peer);
        headers.append("x-forwarded-for", "192.0.2.1".parse().unwrap());
        headers.append("x-forwarded-for", "198.51.100.1".parse().unwrap());
        assert_eq!(
            resolve(peer, &headers, &[peer]),
            "198.51.100.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(resolve(peer, &headers, &[]), peer);
        headers.insert(
            "x-forwarded-for",
            vec!["192.0.2.1"; 33].join(",").parse().unwrap(),
        );
        assert_eq!(resolve(peer, &headers, &[peer]), peer);
    }
}
