use crate::error::NetError;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Port of the broker listener when `[listen] address` names none.
pub const DEFAULT_PORT: u16 = 7443;

/// A validated `[listen] address` (spec 3.8, 17): an explicit IP address with a port. A
/// wildcard address (`0.0.0.0`, `::`, `::ffff:0.0.0.0`) is refused; the one exception is the
/// value `container` inside a container, which listens on all interfaces of the container at
/// [`DEFAULT_PORT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenAddr(SocketAddr);

impl ListenAddr {
    /// Parses a `[listen] address` value. `in_container` says whether this process runs in a
    /// container; outside one, `container` is refused.
    pub fn parse(value: &str, in_container: bool) -> Result<ListenAddr, NetError> {
        let value = value.trim();
        if value == "container" {
            if !in_container {
                return Err(NetError::BadListenAddress(
                    "`container` is accepted only inside a container".into(),
                ));
            }
            return Ok(ListenAddr(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                DEFAULT_PORT,
            )));
        }
        let addr = value
            .parse::<SocketAddr>()
            .or_else(|_| {
                value
                    .parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
            })
            .map_err(|_| {
                NetError::BadListenAddress(format!(
                    "`{value}` is not an IP address with an optional port"
                ))
            })?;
        ListenAddr::explicit(addr)
    }

    /// An explicit address; a wildcard is refused, also in its IPv4-mapped IPv6 form.
    pub fn explicit(addr: SocketAddr) -> Result<ListenAddr, NetError> {
        if addr.ip().is_unspecified() || addr.ip().to_canonical().is_unspecified() {
            return Err(NetError::BadListenAddress(format!(
                "wildcard address {} is not allowed; name the interface address",
                addr.ip()
            )));
        }
        Ok(ListenAddr(addr))
    }

    /// The socket address.
    pub fn socket_addr(&self) -> SocketAddr {
        self.0
    }
}
