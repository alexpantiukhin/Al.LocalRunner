use super::conn::ConnId;
use super::local::LOCAL_RUNNER_PREFIX;
use super::{Client, Engine, Role};
use lr_abstractions::{Frame, MachineKind, PeerAccess};
use lr_core::{zone_of, Endpoint, Zone};
use lr_proto::{
    negotiate, receive, BrokerToHub, Hello, HubToBroker, PeerKind, ProtocolErrorCode, Received,
    Welcome, HEARTBEAT_INTERVAL_S,
};

pub(crate) fn peer_kind(endpoint: &Endpoint) -> PeerKind {
    match endpoint {
        Endpoint::Ipc { .. } => PeerKind::Ipc,
        Endpoint::Addr(_) => PeerKind::Network,
    }
}

enum Who {
    Local(super::LocalClient),
    Member(lr_abstractions::MachineRow),
}

impl Engine {
    pub(crate) fn zone_of_peer(&self, endpoint: &Endpoint) -> Zone {
        match endpoint {
            Endpoint::Ipc { .. } => Zone::Local,
            Endpoint::Addr(a) => {
                let flat: Vec<_> = self.site.networks.values().flatten().copied().collect();
                zone_of(Some(*a), &self.site.broker_addrs, &flat)
            }
        }
    }

    pub(crate) fn welcome(&self, conn: ConnId, endpoint: &Endpoint, hello: &Hello) {
        let version = negotiate(hello.version).unwrap_or(hello.version);
        self.send(
            conn,
            &BrokerToHub::Welcome(Welcome {
                version,
                zone: Some(self.zone_of_peer(endpoint)),
                heartbeat_s: HEARTBEAT_INTERVAL_S,
                broker_lost_kill_s: Some(self.lost_kill_s()),
            }),
        );
    }

    pub(crate) async fn hello(&mut self, conn: ConnId, frame: Frame) {
        let Some(peer) = self.conns.get(&conn).map(|c| c.peer.clone()) else {
            return;
        };
        if !matches!(peer.access, PeerAccess::Local | PeerAccess::Member) {
            return self.refuse(
                conn,
                ProtocolErrorCode::Unauthorized,
                "this connection is not a member of the pool".into(),
            );
        }
        let hello = match receive::<HubToBroker>(frame.as_bytes(), peer_kind(&peer.endpoint)) {
            Ok(Received::Message(HubToBroker::Hello(h))) => h,
            other => {
                let refusal = other.err().and_then(|e| e.refusal());
                let (code, message) = refusal.map_or_else(
                    || {
                        (
                            ProtocolErrorCode::Malformed,
                            "the first message must be hello".to_string(),
                        )
                    },
                    |r| (r.code, r.message),
                );
                return self.refuse(conn, code, message);
            }
        };
        if let Err(refused) = negotiate(hello.version) {
            self.send(conn, &BrokerToHub::Error(refused.into_error()));
            return self.close(conn);
        }
        let who = match (&peer.access, &peer.endpoint, &peer.key) {
            (PeerAccess::Local, Endpoint::Ipc { os_user }, _) => {
                match self.local.admit(os_user, &self.policy.file().local_users) {
                    Some(c) => Who::Local(c),
                    None => {
                        return self.refuse(
                            conn,
                            ProtocolErrorCode::Unauthorized,
                            format!("OS user {os_user} is not in [[local_user]]"),
                        )
                    }
                }
            }
            (PeerAccess::Member, _, Some(key)) => match self.store.machine_by_key(key).await {
                Ok(Some(row)) if row.revoked_at.is_none() => Who::Member(row),
                _ => {
                    return self.refuse(
                        conn,
                        ProtocolErrorCode::Unauthorized,
                        "unknown or revoked key".into(),
                    )
                }
            },
            _ => {
                return self.refuse(
                    conn,
                    ProtocolErrorCode::Unauthorized,
                    "no identity on this connection".into(),
                )
            }
        };
        let wants_runner = hello.capabilities.is_some();
        let kind_ok = match &who {
            Who::Local(..) => true,
            Who::Member(row) => (row.kind == MachineKind::Runner) == wants_runner,
        };
        if !kind_ok {
            return self.refuse(
                conn,
                ProtocolErrorCode::Unauthorized,
                "the key is registered for another role".into(),
            );
        }
        if let Some(caps) = hello.capabilities.clone() {
            let (name, owner, persisted, machine) = match &who {
                Who::Local(c) => (
                    format!("{LOCAL_RUNNER_PREFIX}{}", hello.name),
                    c.key.clone(),
                    false,
                    Some(c.key.clone()),
                ),
                Who::Member(row) => (hello.name.clone(), row.name.clone(), true, None),
            };
            if matches!(who, Who::Member(_)) && hello.name.contains(':') {
                return self.refuse(
                    conn,
                    ProtocolErrorCode::Malformed,
                    format!("runner name {} must not contain a colon", hello.name),
                );
            }
            if let Err(e) = self
                .register_runner(conn, &name, caps, owner, persisted, machine)
                .await
            {
                return self.refuse(conn, e.code, e.message);
            }
            self.welcome(conn, &peer.endpoint, &hello);
            self.resend_cancels(&name).await;
            self.plan().await;
            return;
        }
        let client = match who {
            Who::Local(c) => Client {
                machine_id: c.key.clone(),
                key: c.key,
                label: c.label,
                machine: None,
                owner: c.owner,
                endpoint: peer.endpoint.clone(),
            },
            Who::Member(row) => Client {
                key: row.name.clone(),
                label: row.label,
                machine_id: row.machine_id,
                machine: Some(row.name),
                owner: false,
                endpoint: peer.endpoint.clone(),
            },
        };
        if let Some(name) = &client.machine {
            let _ = self.store.touch_machine(name, self.now()).await;
        }
        self.hubs.insert(client.key.clone(), conn);
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::Hub(client);
        }
        self.welcome(conn, &peer.endpoint, &hello);
    }
}
