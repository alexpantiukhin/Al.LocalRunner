use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Time a new connection has for TLS and the key proof.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connections in TLS or key proof at once, both pools together.
pub const MAX_PENDING_HANDSHAKES: usize = 64;

/// Limits of the broker's TLS listener against connection floods (spec 17).
///
/// A connection holds a handshake slot from TCP accept until the role took the transport
/// (`handshake_timeout` for TLS and the key proof, then `handover_timeout` for the role's
/// `accept`). Slots come from two pools. The `general_slots` pool is open to anyone, at most
/// `per_ip` slots per source. The `reserved_slots` pool serves only sources where a member
/// authenticated before (the last `known_addresses` of them), at most `reserved_per_ip` per
/// source, when the general pool is full or the source used up its `per_ip` share: junk
/// connections, from many sources or from the source of one member, cannot keep the other
/// hubs and runners out. The list of member sources lives in memory only and is empty after a
/// broker restart, so right after a restart every reconnect competes for the general pool. A
/// member that comes back from a new source competes for the general pool too. A connection
/// that finds no slot is dropped at once.
///
/// A source is an IPv4 address, or the /64 network of an IPv6 address (one host usually owns
/// a whole /64).
///
/// A joining connection (unknown key, open join code) additionally holds one of
/// `joining_slots`, at most `joining_per_ip` per source, from its key proof until it closes or
/// `joining_lifetime` passes; without a free one the key proof is answered `rate_limited`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Handshake slots open to every source.
    pub general_slots: usize,
    /// Handshake slots only for sources where a member authenticated before.
    pub reserved_slots: usize,
    /// General slots one source may hold at once.
    pub per_ip: usize,
    /// Reserved slots one source may hold at once.
    pub reserved_per_ip: usize,
    /// Time for TLS and the key proof.
    pub handshake_timeout: Duration,
    /// Time the role has to take an authenticated transport; then it is closed.
    pub handover_timeout: Duration,
    /// Time a joining peer has to send its `join`.
    pub join_timeout: Duration,
    /// Joining connections open at once.
    pub joining_slots: usize,
    /// Joining connections of one source open at once.
    pub joining_per_ip: usize,
    /// Longest life of a joining connection; then the transport shuts it down.
    pub joining_lifetime: Duration,
    /// How many member sources are remembered for the reserved pool.
    pub known_addresses: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            general_slots: MAX_PENDING_HANDSHAKES - 16,
            reserved_slots: 16,
            per_ip: 8,
            reserved_per_ip: 2,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            handover_timeout: HANDSHAKE_TIMEOUT,
            join_timeout: HANDSHAKE_TIMEOUT,
            joining_slots: 16,
            joining_per_ip: 2,
            joining_lifetime: Duration::from_secs(30),
            known_addresses: 1024,
        }
    }
}

/// The source a connection is counted under: an IPv4 address, or an IPv6 /64.
pub(crate) fn source_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let bits = u128::from(v6) & !((1u128 << 64) - 1);
            IpAddr::V6(Ipv6Addr::from(bits))
        }
        v4 => v4,
    }
}

#[derive(Default)]
struct State {
    general_of: HashMap<IpAddr, usize>,
    reserved_of: HashMap<IpAddr, usize>,
    joining_of: HashMap<IpAddr, usize>,
    general: usize,
    reserved: usize,
    joining: usize,
    known: VecDeque<IpAddr>,
}

pub(crate) struct Slots {
    limits: Limits,
    state: Mutex<State>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pool {
    General,
    Reserved,
    Joining,
}

/// A held slot of one pool, given back on drop.
pub(crate) struct Slot {
    slots: Arc<Slots>,
    source: IpAddr,
    pool: Pool,
}

fn take(map: &mut HashMap<IpAddr, usize>, source: IpAddr) {
    *map.entry(source).or_insert(0) += 1;
}

fn give(map: &mut HashMap<IpAddr, usize>, source: IpAddr) {
    if let Some(n) = map.get_mut(&source) {
        *n -= 1;
        if *n == 0 {
            map.remove(&source);
        }
    }
}

fn held(map: &HashMap<IpAddr, usize>, source: IpAddr) -> usize {
    map.get(&source).copied().unwrap_or(0)
}

impl Slots {
    pub(crate) fn new(limits: Limits) -> Arc<Slots> {
        Arc::new(Slots {
            limits,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn slot(self: &Arc<Self>, source: IpAddr, pool: Pool) -> Slot {
        Slot {
            slots: self.clone(),
            source,
            pool,
        }
    }

    /// A handshake slot for a connection from `ip`, or `None`: drop the connection.
    pub(crate) fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<Slot> {
        let source = source_of(ip);
        let l = &self.limits;
        let mut s = self.lock();
        if held(&s.general_of, source) < l.per_ip && s.general < l.general_slots {
            s.general += 1;
            take(&mut s.general_of, source);
            return Some(self.slot(source, Pool::General));
        }
        if s.reserved < l.reserved_slots
            && held(&s.reserved_of, source) < l.reserved_per_ip
            && s.known.contains(&source)
        {
            s.reserved += 1;
            take(&mut s.reserved_of, source);
            return Some(self.slot(source, Pool::Reserved));
        }
        None
    }

    /// A joining slot for an unknown key from `ip`, or `None`: refuse it as rate-limited.
    pub(crate) fn acquire_joining(self: &Arc<Self>, ip: IpAddr) -> Option<Slot> {
        let source = source_of(ip);
        let l = &self.limits;
        let mut s = self.lock();
        if s.joining < l.joining_slots && held(&s.joining_of, source) < l.joining_per_ip {
            s.joining += 1;
            take(&mut s.joining_of, source);
            return Some(self.slot(source, Pool::Joining));
        }
        None
    }

    /// Records that a member authenticated from `ip`.
    pub(crate) fn remember(&self, ip: IpAddr) {
        let source = source_of(ip);
        let mut s = self.lock();
        s.known.retain(|k| *k != source);
        s.known.push_back(source);
        while s.known.len() > self.limits.known_addresses {
            s.known.pop_front();
        }
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.slots.lock();
        let source = self.source;
        match self.pool {
            Pool::General => {
                s.general -= 1;
                give(&mut s.general_of, source);
            }
            Pool::Reserved => {
                s.reserved -= 1;
                give(&mut s.reserved_of, source);
            }
            Pool::Joining => {
                s.joining -= 1;
                give(&mut s.joining_of, source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv6_sources_are_their_64_networks() {
        assert_eq!(
            source_of(ip("2001:db8::1")),
            source_of(ip("2001:db8::ffff:2"))
        );
        assert_ne!(
            source_of(ip("2001:db8::1")),
            source_of(ip("2001:db8:0:1::1"))
        );
        assert_eq!(source_of(ip("192.0.2.7")), ip("192.0.2.7"));
        assert_eq!(source_of(ip("::ffff:192.0.2.7")), ip("192.0.2.7"));
    }

    #[test]
    fn per_source_limit_counts_a_whole_ipv6_64() {
        let slots = Slots::new(Limits {
            per_ip: 2,
            reserved_slots: 0,
            ..Limits::default()
        });
        let a = slots.acquire(ip("2001:db8::1")).unwrap();
        let _b = slots.acquire(ip("2001:db8::2")).unwrap();
        assert!(slots.acquire(ip("2001:db8::3")).is_none());
        assert!(slots.acquire(ip("2001:db8:0:1::3")).is_some());
        drop(a);
        assert!(slots.acquire(ip("2001:db8::3")).is_some());
    }

    #[test]
    fn joining_slots_are_capped_globally_and_per_source() {
        let slots = Slots::new(Limits {
            joining_slots: 3,
            joining_per_ip: 2,
            ..Limits::default()
        });
        let a = slots.acquire_joining(ip("192.0.2.1")).unwrap();
        let _b = slots.acquire_joining(ip("192.0.2.1")).unwrap();
        assert!(slots.acquire_joining(ip("192.0.2.1")).is_none());
        let _c = slots.acquire_joining(ip("192.0.2.2")).unwrap();
        assert!(slots.acquire_joining(ip("192.0.2.3")).is_none());
        drop(a);
        assert!(slots.acquire_joining(ip("192.0.2.3")).is_some());
    }
}
