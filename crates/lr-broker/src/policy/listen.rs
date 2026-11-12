use super::error::PolicyError;
use ipnet::IpNet;
use lr_core::NamedNetworks;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

/// One `[listen] address` value (spec 3.8, 10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenValue {
    /// An explicit interface address with an optional port; without one the listener's
    /// default port applies.
    Explicit(IpAddr, Option<u16>),
    /// `container`: all interfaces of the container; valid only inside a container.
    Container,
}

impl ListenValue {
    fn parse(value: &str) -> Result<ListenValue, String> {
        let value = value.trim();
        if value == "container" {
            return Ok(ListenValue::Container);
        }
        let (ip, port) = match value.parse::<SocketAddr>() {
            Ok(sa) => (sa.ip(), Some(sa.port())),
            Err(_) => match value.parse::<IpAddr>() {
                Ok(ip) => (ip, None),
                Err(_) => {
                    return Err(format!(
                        "`{value}` is not an IP address with an optional port or `container`"
                    ))
                }
            },
        };
        if ip.is_unspecified() || ip.to_canonical().is_unspecified() {
            return Err(format!(
                "wildcard address {ip} is not allowed; name the interface address \
                 (`container` inside a container)"
            ));
        }
        Ok(ListenValue::Explicit(ip, port))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

fn addresses<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ListenValue>, D::Error> {
    let values = match OneOrMany::deserialize(d)? {
        OneOrMany::One(v) => vec![v],
        OneOrMany::Many(v) => v,
    };
    let parsed = values
        .iter()
        .map(|v| ListenValue::parse(v))
        .collect::<Result<Vec<_>, _>>()
        .map_err(D::Error::custom)?;
    if parsed.len() > 1 && parsed.contains(&ListenValue::Container) {
        return Err(D::Error::custom(
            "`container` must be the only listen address",
        ));
    }
    Ok(parsed)
}

/// `[listen]`: read only when the service starts; `policy reload` reports changes and does not
/// apply them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenSection {
    /// `address`: one value or a list, in order; the first is printed by `broker add`. Empty:
    /// the broker listens on no network address (there is no default address).
    #[serde(rename = "address", deserialize_with = "addresses")]
    pub addresses: Vec<ListenValue>,
}

impl ListenSection {
    /// The start-time check: `container` is accepted only when `in_container` is true.
    pub fn check_at_start(&self, in_container: bool) -> Result<(), PolicyError> {
        if !in_container && self.addresses.contains(&ListenValue::Container) {
            return Err(PolicyError::at(
                "listen",
                Some("address"),
                None,
                "`container` is accepted only inside a container".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Range(IpNet);

impl TryFrom<String> for Range {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        let s = s.trim();
        s.parse::<IpNet>()
            .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
            .map(Range)
            .map_err(|_| format!("`{s}` is not an address range (CIDR) or an address"))
    }
}

pub(crate) fn networks<'de, D: Deserializer<'de>>(d: D) -> Result<NamedNetworks, D::Error> {
    let raw = BTreeMap::<String, Vec<Range>>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(name, ranges)| (name, ranges.into_iter().map(|r| r.0).collect()))
        .collect())
}
