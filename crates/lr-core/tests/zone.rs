use ipnet::IpNet;
use lr_core::{zone_of, Zone};
use std::net::IpAddr;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn nets(list: &[&str]) -> Vec<IpNet> {
    list.iter().map(|s| s.parse().unwrap()).collect()
}

fn home() -> Vec<IpNet> {
    nets(&["192.168.1.0/24", "127.0.0.1/32", "172.17.0.0/16"])
}

#[test]
fn ipc_of_the_same_user_is_local() {
    assert_eq!(zone_of(None, &[ip("192.168.1.5")], &home()), Zone::Local);
}

#[test]
fn address_in_networks_is_lan_else_remote() {
    let host = [ip("192.168.1.5")];
    assert_eq!(zone_of(Some(ip("192.168.1.77")), &host, &home()), Zone::Lan);
    assert_eq!(zone_of(Some(ip("172.17.0.9")), &host, &home()), Zone::Lan);
    assert_eq!(zone_of(Some(ip("10.0.0.1")), &host, &home()), Zone::Remote);
    assert_eq!(
        zone_of(Some(ip("192.168.2.1")), &host, &home()),
        Zone::Remote
    );
    assert_eq!(zone_of(Some(ip("192.168.1.77")), &host, &[]), Zone::Remote);
}

#[test]
fn other_os_user_ip_counts_as_broker_host_zone() {
    let host = [ip("192.168.1.5")];
    assert_eq!(zone_of(Some(ip("192.168.1.5")), &host, &home()), Zone::Lan);
    assert_eq!(zone_of(Some(ip("127.0.0.1")), &host, &home()), Zone::Lan);
}

#[test]
fn host_address_outside_networks_is_remote_not_local() {
    let host = [ip("10.9.9.9")];
    assert_eq!(zone_of(Some(ip("10.9.9.9")), &host, &home()), Zone::Remote);
}

#[test]
fn loopback_follows_the_hosts_network_even_when_not_listed() {
    let host = [ip("192.168.1.5")];
    let only_lan = nets(&["192.168.1.0/24"]);
    assert_eq!(zone_of(Some(ip("127.0.0.1")), &host, &only_lan), Zone::Lan);
    assert_eq!(zone_of(Some(ip("::1")), &host, &only_lan), Zone::Lan);
}

#[test]
fn ipv6_networks_are_matched() {
    let n = nets(&["fd00::/8"]);
    assert_eq!(zone_of(Some(ip("fd12::1")), &[], &n), Zone::Lan);
    assert_eq!(zone_of(Some(ip("2001:db8::1")), &[], &n), Zone::Remote);
}

#[test]
fn zone_names_round_trip() {
    for z in [Zone::Local, Zone::Lan, Zone::Remote] {
        assert_eq!(z.as_str().parse::<Zone>().unwrap(), z);
    }
    assert!(Zone::Local < Zone::Lan && Zone::Lan < Zone::Remote);
}

mod pairwise {
    use super::ip;
    use lr_core::{network_of, zone_between, Endpoint, NamedNetworks, Zone};

    fn named() -> NamedNetworks {
        let mut n = NamedNetworks::new();
        n.insert(
            "home".into(),
            vec![
                "192.168.1.0/24".parse().unwrap(),
                "127.0.0.1/32".parse().unwrap(),
                "172.17.0.0/16".parse().unwrap(),
            ],
        );
        n.insert("office".into(), vec!["10.1.0.0/16".parse().unwrap()]);
        n
    }

    fn addr(s: &str) -> Endpoint {
        Endpoint::Addr(ip(s))
    }

    fn ipc(user: &str) -> Endpoint {
        Endpoint::Ipc {
            os_user: user.into(),
        }
    }

    const HOST: &str = "192.168.1.10";

    fn zone(a: &Endpoint, b: &Endpoint) -> Zone {
        zone_between(a, b, &[ip(HOST)], &named())
    }

    #[test]
    fn network_of_names_every_containing_network() {
        let n = named();
        assert_eq!(
            network_of(ip("192.168.1.7"), &n)
                .into_iter()
                .collect::<Vec<_>>(),
            ["home"]
        );
        assert!(network_of(ip("8.8.8.8"), &n).is_empty());
        let mut overlap = named();
        overlap.insert("lab".into(), vec!["192.168.1.0/25".parse().unwrap()]);
        assert_eq!(network_of(ip("192.168.1.7"), &overlap).len(), 2);
    }

    #[test]
    fn same_os_user_over_ipc_is_local() {
        assert_eq!(zone(&ipc("alex"), &ipc("alex")), Zone::Local);
    }

    #[test]
    fn other_os_user_over_ipc_is_the_host_network() {
        assert_eq!(zone(&ipc("alex"), &ipc("bob")), Zone::Lan);
        assert_eq!(zone(&ipc("alex"), &addr("192.168.1.77")), Zone::Lan);
        assert_eq!(zone(&ipc("alex"), &addr("10.1.0.5")), Zone::Remote);
        let none = NamedNetworks::new();
        assert_eq!(
            zone_between(&ipc("alex"), &ipc("bob"), &[ip(HOST)], &none),
            Zone::Remote
        );
    }

    #[test]
    fn both_addresses_in_one_named_network_is_lan() {
        assert_eq!(
            zone(&addr("192.168.1.50"), &addr("192.168.1.20")),
            Zone::Lan
        );
        assert_eq!(zone(&addr("10.1.2.3"), &addr("10.1.9.9")), Zone::Lan);
        assert_eq!(zone(&addr("192.168.1.50"), &addr("172.17.0.4")), Zone::Lan);
    }

    #[test]
    fn addresses_in_different_named_networks_are_remote() {
        assert_eq!(zone(&addr("192.168.1.50"), &addr("10.1.2.3")), Zone::Remote);
        assert_eq!(zone(&addr("192.168.1.50"), &addr("8.8.8.8")), Zone::Remote);
        assert_eq!(zone(&addr("8.8.8.8"), &addr("8.8.4.4")), Zone::Remote);
    }

    #[test]
    fn network_connection_of_the_same_user_is_not_local() {
        assert_eq!(zone(&addr("127.0.0.1"), &ipc("alex")), Zone::Lan);
        assert_eq!(zone(&addr(HOST), &addr("192.168.1.20")), Zone::Lan);
    }

    #[test]
    fn host_and_loopback_belong_to_the_host_network_even_when_unlisted() {
        let mut only_lan = NamedNetworks::new();
        only_lan.insert("home".into(), vec!["192.168.1.0/24".parse().unwrap()]);
        assert_eq!(
            zone_between(&addr("::1"), &addr("192.168.1.20"), &[ip(HOST)], &only_lan),
            Zone::Lan
        );
    }
}
