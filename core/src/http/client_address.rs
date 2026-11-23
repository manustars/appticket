//! The address of the client a request comes from, which failed sign-ins are counted under:
//! the connection's, or behind trusted reverse proxies the one they forward.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::{HeaderValue, request::Parts};
use ipnet::IpNet;

use super::{ApiError, AppState};

/// The reverse proxies whose `X-Forwarded-For` is believed. Empty: the header is ignored.
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies(Arc<[IpNet]>);

impl TrustedProxies {
    pub fn new(proxies: Vec<IpNet>) -> TrustedProxies {
        TrustedProxies(proxies.into())
    }

    /// The client's address, for a connection from `peer` carrying the `X-Forwarded-For` header
    /// lines `forwarded_for`. Each proxy adds the address it was connected from on the right,
    /// and the client can write anything on the left: the client is the first address from the
    /// right that is not a trusted proxy.
    pub fn client_address<'a>(
        &self,
        peer: IpAddr,
        forwarded_for: impl Iterator<Item = &'a HeaderValue>,
    ) -> IpAddr {
        let mut client = peer.to_canonical();
        if !self.trusts(client) {
            return client;
        }
        let entries: Vec<Option<IpAddr>> = forwarded_for
            .flat_map(|line| match line.to_str() {
                Ok(text) => text.split(',').map(entry_address).collect(),
                Err(_) => vec![None],
            })
            .collect();
        for entry in entries.into_iter().rev() {
            // An entry that is no address was not written by a trusted proxy: the client is
            // counted under the proxy that passed it on.
            let Some(address) = entry else {
                return client;
            };
            client = address;
            if !self.trusts(client) {
                return client;
            }
        }
        client
    }

    fn trusts(&self, address: IpAddr) -> bool {
        self.0.iter().any(|proxy| proxy.contains(&address))
    }
}

/// An entry of `X-Forwarded-For`: an address, possibly with a port (`[2001:db8::1]:443`).
fn entry_address(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<SocketAddr>().map(|socket| socket.ip()))
        .ok()
        .map(|address| address.to_canonical())
}

/// One trusted proxy: an address, or a network such as `192.0.2.0/24`.
pub fn parse_proxy(text: &str) -> Result<IpNet, String> {
    let text = text.trim();
    text.parse::<IpNet>()
        .or_else(|_| text.parse::<IpAddr>().map(IpNet::from))
        .map(|net| net.trunc())
        .map_err(|_| {
            format!("{text:?} is neither an IP address nor a network such as 192.0.2.0/24")
        })
}

/// The address of the client a request comes from.
pub struct ClientAddress(pub IpAddr);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // The server always records the connection's address; without it, failures would all
        // be counted under one address.
        let Some(ConnectInfo(peer)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() else {
            return Err(ApiError::Internal(anyhow::anyhow!(
                "the request carries no connection address"
            )));
        };
        Ok(ClientAddress(state.trusted_proxies.client_address(
            peer.ip(),
            parts.headers.get_all("x-forwarded-for").iter(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxies(list: &[&str]) -> TrustedProxies {
        TrustedProxies::new(list.iter().map(|p| parse_proxy(p).unwrap()).collect())
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// The client seen through `trusted`, connected from `peer` with these header lines.
    fn client(trusted: &[&str], peer: &str, lines: &[&str]) -> IpAddr {
        let lines: Vec<HeaderValue> = lines
            .iter()
            .map(|l| HeaderValue::from_str(l).unwrap())
            .collect();
        proxies(trusted).client_address(ip(peer), lines.iter())
    }

    #[test]
    fn forwarded_addresses_are_ignored_when_no_proxy_is_trusted() {
        assert_eq!(
            client(&[], "192.0.2.10", &["203.0.113.7"]),
            ip("192.0.2.10")
        );
    }

    #[test]
    fn forwarded_addresses_are_ignored_from_a_connection_that_is_not_a_trusted_proxy() {
        assert_eq!(
            client(&["192.0.2.0/24"], "198.51.100.4", &["203.0.113.7"]),
            ip("198.51.100.4")
        );
    }

    /// The leftmost entries are whatever the client sent; only what trusted proxies added counts.
    #[test]
    fn the_client_is_the_first_address_from_the_right_that_is_not_a_trusted_proxy() {
        assert_eq!(
            client(
                &["192.0.2.0/24"],
                "192.0.2.10",
                &["198.51.100.66, 203.0.113.7, 192.0.2.20"]
            ),
            ip("203.0.113.7")
        );
    }

    #[test]
    fn several_header_lines_are_read_in_order() {
        assert_eq!(
            client(
                &["192.0.2.10", "192.0.2.20"],
                "192.0.2.10",
                &["198.51.100.66", "203.0.113.7", "192.0.2.20"]
            ),
            ip("203.0.113.7")
        );
    }

    /// An entry that is no address may be anything the client made up: the client is counted
    /// under the trusted proxy that passed it on.
    #[test]
    fn an_entry_that_is_no_address_stops_at_the_proxy_that_passed_it_on() {
        assert_eq!(
            client(
                &["192.0.2.0/24"],
                "192.0.2.10",
                &["203.0.113.7, unknown, 192.0.2.20"]
            ),
            ip("192.0.2.20")
        );
    }

    #[test]
    fn a_request_from_trusted_proxies_only_is_counted_under_the_first_of_them() {
        assert_eq!(
            client(&["192.0.2.0/24"], "192.0.2.10", &["192.0.2.30, 192.0.2.20"]),
            ip("192.0.2.30")
        );
        assert_eq!(
            client(&["192.0.2.0/24"], "192.0.2.10", &[]),
            ip("192.0.2.10")
        );
    }

    #[test]
    fn addresses_with_a_port_and_mapped_ipv4_are_understood() {
        assert_eq!(
            client(
                &["192.0.2.0/24", "2001:db8:1::/48"],
                "::ffff:192.0.2.10",
                &["[2001:db8:9::5]:4711, 2001:db8:1::2, 192.0.2.20:443"]
            ),
            ip("2001:db8:9::5")
        );
    }

    #[test]
    fn a_trusted_proxy_is_an_address_or_a_network() {
        assert_eq!(
            parse_proxy(" 192.0.2.7 ").unwrap().to_string(),
            "192.0.2.7/32"
        );
        assert_eq!(
            parse_proxy("192.0.2.7/24").unwrap().to_string(),
            "192.0.2.0/24"
        );
        assert_eq!(
            parse_proxy("2001:db8::/32").unwrap().to_string(),
            "2001:db8::/32"
        );
        assert!(parse_proxy("proxy.example.com").is_err());
    }
}
