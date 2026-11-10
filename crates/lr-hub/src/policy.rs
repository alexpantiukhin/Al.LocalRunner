//! `hub-policy.toml` (spec 3.5): which known broker gets an order and how long an order waits
//! in the outbox.

use lr_core::Zone;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;

/// Largest `query_timeout_s` and `query_retry_s`: one hour.
pub const MAX_QUERY_S: u64 = 3600;
/// Largest `query_attempts`.
pub const MAX_QUERY_ATTEMPTS: u32 = 100;
/// Largest `ttl_hours`: one year.
pub const MAX_TTL_HOURS: u64 = 8760;

/// `[brokers] strategy`: how to choose among suitable brokers of one zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerStrategy {
    /// The first suitable broker of `order`; the rest are spares (the default).
    First,
    /// The suitable broker with the most online suitable runners; ties go by `order`.
    LeastLoaded,
}

/// `[brokers] zone_query`: zones one after another or all at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneQuery {
    /// Query the next zone only when the nearer one had no suitable broker (the default).
    Sequential,
    /// Query every allowed zone at once; the choice still follows `broker_zone_order`.
    All,
}

/// `[outbox] on_expire`: what happens to an order that waited `ttl_hours` without `Accepted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExpire {
    /// The order is rejected with `rejected_outbox_expired` (the default).
    Fail,
    /// The order keeps waiting.
    Keep,
}

/// `[brokers]` of `hub-policy.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokersPolicy {
    /// Broker names in order of preference; brokers not named follow by name.
    pub order: Vec<String>,
    /// Choice among suitable brokers of one zone.
    pub strategy: BrokerStrategy,
    /// Order in which zones are queried: a permutation of local, lan and remote.
    pub broker_zone_order: Vec<Zone>,
    /// Seconds to wait for a capabilities reply (default 3).
    pub query_timeout_s: u64,
    /// Attempts of the whole choice, the first included (default 3).
    pub query_attempts: u32,
    /// Zones one after another or all at once.
    pub zone_query: ZoneQuery,
    /// Seconds between attempts (default 5).
    pub query_retry_s: u64,
}

/// `[outbox]` of `hub-policy.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxPolicy {
    /// Hours an order waits for a broker's `Accepted` (default 24).
    pub ttl_hours: u64,
    /// What happens after `ttl_hours`.
    pub on_expire: OnExpire,
}

/// The whole `hub-policy.toml`; an empty file gives [`HubPolicy::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubPolicy {
    /// `[brokers]`.
    pub brokers: BrokersPolicy,
    /// `[outbox]`.
    pub outbox: OutboxPolicy,
}

impl Default for HubPolicy {
    fn default() -> HubPolicy {
        HubPolicy {
            brokers: BrokersPolicy {
                order: Vec::new(),
                strategy: BrokerStrategy::First,
                broker_zone_order: vec![Zone::Local, Zone::Lan, Zone::Remote],
                query_timeout_s: 3,
                query_attempts: 3,
                zone_query: ZoneQuery::Sequential,
                query_retry_s: 5,
            },
            outbox: OutboxPolicy {
                ttl_hours: 24,
                on_expire: OnExpire::Fail,
            },
        }
    }
}

/// Why `hub-policy.toml` was refused; the text names the table, the key and the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubPolicyError(pub String);

impl fmt::Display for HubPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hub-policy.toml: {}", self.0)
    }
}

impl std::error::Error for HubPolicyError {}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    brokers: RawBrokers,
    #[serde(default)]
    outbox: RawOutbox,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawBrokers {
    order: Option<Vec<String>>,
    strategy: Option<String>,
    broker_zone_order: Option<Vec<String>>,
    query_timeout_s: Option<u64>,
    query_attempts: Option<u32>,
    zone_query: Option<String>,
    query_retry_s: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawOutbox {
    ttl_hours: Option<u64>,
    on_expire: Option<String>,
}

fn bad(key: &str, why: impl fmt::Display) -> HubPolicyError {
    HubPolicyError(format!("{key}: {why}"))
}

fn ranged<T: PartialOrd + Copy + fmt::Display>(
    key: &str,
    value: Option<T>,
    default: T,
    min: T,
    max: T,
) -> Result<T, HubPolicyError> {
    let v = value.unwrap_or(default);
    if v < min || v > max {
        return Err(bad(key, format!("{v} is outside {min}..={max}")));
    }
    Ok(v)
}

fn zone_order(raw: Option<Vec<String>>, default: Vec<Zone>) -> Result<Vec<Zone>, HubPolicyError> {
    let key = "[brokers] broker_zone_order";
    let Some(raw) = raw else {
        return Ok(default);
    };
    let zones: Vec<Zone> = raw
        .iter()
        .map(|z| z.parse::<Zone>().map_err(|e| bad(key, e)))
        .collect::<Result<_, _>>()?;
    let distinct: BTreeSet<Zone> = zones.iter().copied().collect();
    if zones.len() != 3 || distinct.len() != 3 {
        return Err(bad(
            key,
            "must list local, lan and remote, each exactly once",
        ));
    }
    Ok(zones)
}

/// Parses `hub-policy.toml`. An unknown table or key, a wrong type or a value out of range is
/// refused with the table, the key and the line; missing keys take their defaults.
pub fn parse_hub_policy(text: &str) -> Result<HubPolicy, HubPolicyError> {
    let raw: RawFile = toml::from_str(text).map_err(|e| HubPolicyError(e.to_string()))?;
    let d = HubPolicy::default();
    let order = raw.brokers.order.unwrap_or_default();
    let mut seen = BTreeSet::new();
    for name in &order {
        if name.trim().is_empty() || !seen.insert(name.as_str()) {
            return Err(bad(
                "[brokers] order",
                format!("broker name `{name}` is empty or repeated"),
            ));
        }
    }
    let strategy = match raw.brokers.strategy.as_deref() {
        None | Some("first") => BrokerStrategy::First,
        Some("least_loaded") => BrokerStrategy::LeastLoaded,
        Some(other) => {
            return Err(bad(
                "[brokers] strategy",
                format!("unknown strategy `{other}`, expected first or least_loaded"),
            ))
        }
    };
    let zone_query = match raw.brokers.zone_query.as_deref() {
        None | Some("sequential") => ZoneQuery::Sequential,
        Some("all") => ZoneQuery::All,
        Some(other) => {
            return Err(bad(
                "[brokers] zone_query",
                format!("unknown value `{other}`, expected sequential or all"),
            ))
        }
    };
    let on_expire = match raw.outbox.on_expire.as_deref() {
        None | Some("fail") => OnExpire::Fail,
        Some("keep") => OnExpire::Keep,
        Some(other) => {
            return Err(bad(
                "[outbox] on_expire",
                format!("unknown value `{other}`, expected fail or keep"),
            ))
        }
    };
    Ok(HubPolicy {
        brokers: BrokersPolicy {
            order,
            strategy,
            broker_zone_order: zone_order(
                raw.brokers.broker_zone_order,
                d.brokers.broker_zone_order,
            )?,
            query_timeout_s: ranged(
                "[brokers] query_timeout_s",
                raw.brokers.query_timeout_s,
                d.brokers.query_timeout_s,
                1,
                MAX_QUERY_S,
            )?,
            query_attempts: ranged(
                "[brokers] query_attempts",
                raw.brokers.query_attempts,
                d.brokers.query_attempts,
                1,
                MAX_QUERY_ATTEMPTS,
            )?,
            zone_query,
            query_retry_s: ranged(
                "[brokers] query_retry_s",
                raw.brokers.query_retry_s,
                d.brokers.query_retry_s,
                0,
                MAX_QUERY_S,
            )?,
        },
        outbox: OutboxPolicy {
            ttl_hours: ranged(
                "[outbox] ttl_hours",
                raw.outbox.ttl_hours,
                d.outbox.ttl_hours,
                1,
                MAX_TTL_HOURS,
            )?,
            on_expire,
        },
    })
}
