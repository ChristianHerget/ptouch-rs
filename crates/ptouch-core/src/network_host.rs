// SPDX-License-Identifier: MIT
//! Host-field validation around Rust's address parser and system resolver.
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6, ToSocketAddrs};

use crate::{PtouchError, Result};

fn invalid(message: &str) -> PtouchError {
    PtouchError::StatusError(message.into())
}

/// Validate without DNS or printer access. Return an unbracketed host suitable
/// for `ToSocketAddrs`; ports are supplied separately by the protocol.
pub fn normalize(host: &str) -> Result<&str> {
    if host.is_empty()
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
        || host.contains(['/', '\\', '@', '?', '#'])
    {
        return Err(invalid(
            "Use a hostname or IP address, without a URL, port, or whitespace",
        ));
    }
    let bracketed = host.starts_with('[');
    let host = if bracketed {
        host.strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .ok_or_else(|| invalid("IPv6 brackets must be one matching pair without a port"))?
    } else {
        host
    };
    if host.is_empty() || host.contains(['[', ']']) {
        return Err(invalid("Invalid address brackets"));
    }
    let (address, scope) = match host.split_once('%') {
        Some((address, scope)) => (address, Some(scope)),
        None => (host, None),
    };
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => {
            if let Some(scope) = scope {
                if scope.is_empty()
                    || !scope
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                    || (scope.bytes().all(|c| c.is_ascii_digit())
                        && scope.parse::<u32>().ok().is_none_or(|n| n == 0))
                {
                    return Err(invalid(
                        "Use a nonzero IPv6 interface index or a local interface name after %",
                    ));
                }
            } else if ip.is_unicast_link_local() {
                return Err(invalid(
                    "Link-local IPv6 needs a local interface scope, for example fe80::1%3",
                ));
            }
        }
        Ok(IpAddr::V4(_)) if !bracketed && scope.is_none() => {}
        Err(_) if !bracketed && scope.is_none() && !host.contains(':') => {}
        _ => return Err(invalid("Use a hostname or valid IP address without a port")),
    }
    Ok(host)
}

/// Preserve system address ordering and IPv6 scope. Named scopes are resolved
/// by the OS; numeric scopes do not require name resolution.
pub fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let host = normalize(host)?;
    if let Some((ip, scope)) = host.split_once('%')
        && let (Ok(ip), Ok(scope)) = (ip.parse::<Ipv6Addr>(), scope.parse::<u32>())
    {
        return Ok(vec![SocketAddrV6::new(ip, port, 0, scope).into()]);
    }
    let mut addresses = Vec::new();
    for address in (host, port)
        .to_socket_addrs()
        .map_err(|e| invalid(&format!("Cannot resolve {host}: {e}")))?
    {
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return Err(invalid("Host resolved to no IP addresses"));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_hosts_and_ip_literals_without_resolving() {
        for host in [
            "printer.local",
            "printer",
            "192.0.2.1",
            "2001:db8::1",
            "[fd00::1]",
            "fe80::1%eth0",
            "[fe80::1%12]",
        ] {
            assert!(normalize(host).is_ok(), "{host}");
        }
        for host in [
            "",
            " printer",
            "http://printer",
            "printer:515",
            "192.0.2.1:515",
            "[::1]:515",
            "[[::1]]",
            "[::1",
            "::1]",
            "[printer]",
            "[192.0.2.1]",
            "fe80::1",
            "fe80::1%",
            "fe80::1%0",
            "fe80::1%4294967296",
            "printer%eth0",
        ] {
            assert!(normalize(host).is_err(), "{host}");
        }
    }

    #[test]
    fn numeric_literals_preserve_family_scope_and_protocol_port() {
        assert_eq!(
            resolve("192.0.2.1", 161).unwrap(),
            vec!["192.0.2.1:161".parse::<SocketAddr>().unwrap()]
        );
        let mut address = resolve("[fe80::1%12]", 161).unwrap()[0];
        address.set_port(515);
        assert_eq!(
            address,
            SocketAddrV6::new("fe80::1".parse().unwrap(), 515, 0, 12).into()
        );
        assert!(
            resolve("localhost", 161)
                .unwrap()
                .iter()
                .all(|a| a.ip().is_loopback())
        );
    }
}
