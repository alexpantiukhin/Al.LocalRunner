use crate::model::Zone;
use ipnet::IpNet;
use std::net::IpAddr;

/// Zone of ONE endpoint as the broker sees it (spec 3.12), judged against the flat `networks`
/// list. This is not the pairwise client-to-runner relation of spec 3.12 (both addresses in the
/// same named network); that rule belongs to the scheduler and routing.
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
