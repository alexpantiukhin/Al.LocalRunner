use crate::model::Zone;
use ipnet::IpNet;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

/// `[networks]` of the broker: network name to its address ranges (spec 3.5, 3.7).
pub type NamedNetworks = BTreeMap<String, Vec<IpNet>>;

/// How a client or a runner is connected to the broker, as the broker sees it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// Local IPC; the OS user is checked by the OS kernel.
    Ipc {
        /// OS user of the peer.
        os_user: String,
    },
    /// Network connection from this source address.
    Addr(IpAddr),
}

/// Where the broker stands: its host addresses and its `[networks]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Site {
    /// Addresses of the broker host.
    pub broker_addrs: Vec<IpAddr>,
    /// Named networks of the broker.
    pub networks: NamedNetworks,
}

impl Site {
    /// Pairwise zone of `runner` relative to `client`, see [`zone_between`].
    pub fn zone_between(&self, client: &Endpoint, runner: &Endpoint) -> Zone {
        zone_between(client, runner, &self.broker_addrs, &self.networks)
    }
}

/// Zone of ONE endpoint as the broker sees it (spec 3.12), judged against the flat `networks`
/// list. This is not the pairwise client-to-runner relation of spec 3.7 (both addresses in the
/// same named network); that rule is [`zone_between`].
///
/// `addr` is the source address of the connection, `None` for local IPC of the broker's own
/// OS user. IPC from another OS user of the machine is reported by the broker as the host's own
/// address and so is classified like the host: the zone of the network where the host stands.
/// `broker_addr` are the addresses of the broker host; a loopback address belongs to the host
/// too. Any other address is `Lan` when it falls into one of `networks`, else `Remote`.
pub fn zone_of(addr: Option<IpAddr>, broker_addr: &[IpAddr], networks: &[IpNet]) -> Zone {
    let Some(addr) = addr else {
        return Zone::Local;
    };
    let in_networks = |a: &IpAddr| networks.iter().any(|n| n.contains(a));
    let is_host = addr.is_loopback() || broker_addr.contains(&addr);
    let lan = if is_host {
        in_networks(&addr) || broker_addr.iter().any(in_networks)
    } else {
        in_networks(&addr)
    };
    if lan {
        Zone::Lan
    } else {
        Zone::Remote
    }
}

/// Names of the networks of `networks` that contain `addr`.
pub fn network_of(addr: IpAddr, networks: &NamedNetworks) -> BTreeSet<&str> {
    networks
        .iter()
        .filter(|(_, ranges)| ranges.iter().any(|n| n.contains(&addr)))
        .map(|(name, _)| name.as_str())
        .collect()
}

fn endpoint_networks<'a>(
    endpoint: &Endpoint,
    broker_addrs: &[IpAddr],
    networks: &'a NamedNetworks,
) -> BTreeSet<&'a str> {
    let host = || {
        broker_addrs
            .iter()
            .flat_map(|a| network_of(*a, networks))
            .collect::<BTreeSet<_>>()
    };
    match endpoint {
        Endpoint::Ipc { .. } => host(),
        Endpoint::Addr(a) if a.is_loopback() || broker_addrs.contains(a) => {
            let mut nets = host();
            nets.extend(network_of(*a, networks));
            nets
        }
        Endpoint::Addr(a) => network_of(*a, networks),
    }
}

/// Zone of `runner` relative to `client` (spec 3.7): `Local` when both are connected over local
/// IPC by the same OS user; `Lan` when both fall into one named network of `networks`; `Remote`
/// otherwise.
///
/// IPC of another OS user, the broker host's own addresses and loopback count as the host: they
/// belong to every network that contains an address of the host.
pub fn zone_between(
    client: &Endpoint,
    runner: &Endpoint,
    broker_addrs: &[IpAddr],
    networks: &NamedNetworks,
) -> Zone {
    if let (Endpoint::Ipc { os_user: a }, Endpoint::Ipc { os_user: b }) = (client, runner) {
        if a == b {
            return Zone::Local;
        }
    }
    let c = endpoint_networks(client, broker_addrs, networks);
    let r = endpoint_networks(runner, broker_addrs, networks);
    if c.intersection(&r).next().is_some() {
        Zone::Lan
    } else {
        Zone::Remote
    }
}
