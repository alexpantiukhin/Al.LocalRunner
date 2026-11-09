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
