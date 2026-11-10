use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Time a new connection has for TLS and the key proof.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connections in TLS or key proof at once, both pools together.
pub const MAX_PENDING_HANDSHAKES: usize = 64;

/// Limits of the broker's TLS listener against connection floods (spec 17).
///
/// A connection holds a handshake slot from TCP accept until its key proof ends or
/// `handshake_timeout` passes. Slots come from two pools. The `general_slots` pool is open to
/// anyone, at most `per_ip` slots per source address. The `reserved_slots` pool serves only
/// source addresses where a member authenticated before (the last `known_addresses` of them),
/// when the general pool is full or the address used up its `per_ip` share: junk connections,
/// from many addresses or from the address of a member, cannot keep reconnecting hubs and
/// runners out. A member that comes back from a new address competes for the general pool. A
/// connection that finds no slot is dropped at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Slots open to every source address.
    pub general_slots: usize,
    /// Slots only for addresses where a member authenticated before.
    pub reserved_slots: usize,
    /// Slots one source address may hold at once.
    pub per_ip: usize,
    /// Time for TLS and the key proof.
    pub handshake_timeout: Duration,
    /// Time a joining peer has to send its `join`.
    pub join_timeout: Duration,
    /// How many member addresses are remembered for the reserved pool.
    pub known_addresses: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            general_slots: MAX_PENDING_HANDSHAKES - 16,
            reserved_slots: 16,
            per_ip: 8,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            join_timeout: HANDSHAKE_TIMEOUT,
            known_addresses: 1024,
        }
    }
}

#[derive(Default)]
struct State {
    per_ip: HashMap<IpAddr, usize>,
    general: usize,
    reserved: usize,
    known: VecDeque<IpAddr>,
}

pub(crate) struct Slots {
    limits: Limits,
    state: Mutex<State>,
}

pub(crate) struct Slot {
    slots: Arc<Slots>,
    ip: IpAddr,
    reserved: bool,
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

    pub(crate) fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<Slot> {
        let mut s = self.lock();
        let held = s.per_ip.get(&ip).copied().unwrap_or(0);
        let reserved = if held < self.limits.per_ip && s.general < self.limits.general_slots {
            s.general += 1;
            false
        } else if s.reserved < self.limits.reserved_slots && s.known.contains(&ip) {
            s.reserved += 1;
            true
        } else {
            return None;
        };
        *s.per_ip.entry(ip).or_insert(0) += 1;
        Some(Slot {
            slots: self.clone(),
            ip,
            reserved,
        })
    }

    pub(crate) fn remember(&self, ip: IpAddr) {
        let mut s = self.lock();
        s.known.retain(|k| *k != ip);
        s.known.push_back(ip);
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
        if self.reserved {
            s.reserved -= 1;
        } else {
            s.general -= 1;
        }
        if let Some(n) = s.per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                s.per_ip.remove(&self.ip);
            }
        }
    }
}
