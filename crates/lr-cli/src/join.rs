//! Joining a pool with a one-time code (spec 3.8): the broker's desk that redeems a join on a
//! connection the registry admitted as `Joining`, and the joiner's side used by `hub join` and
//! `runner join`.

use async_trait::async_trait;
use lr_abstractions::{
    BrokerStore, Clock, Dialer, Frame, Listener, MachineKind, PeerAccess, Transport, TransportError,
};
use lr_net::{ClientKey, JoinCode, JoinRequest, Registry, TlsDialer};
use lr_proto::{
    decode, negotiate, BrokerToHub, BrokerToRunner, HubToBroker, Join, Joined, ProtocolError,
    ProtocolErrorCode, Role, RunnerToBroker, HEARTBEAT_INTERVAL_S, PROTOCOL_VERSION,
};
use std::sync::Arc;
use std::time::Duration;

const JOIN_WITHIN: Duration = Duration::from_secs(30);

/// Redeems joins for the broker: one `join` per connection, one reply, then the connection
/// closes and the joiner reconnects with its key.
pub struct JoinDesk {
    registry: Arc<Registry>,
    store: Arc<dyn BrokerStore>,
    clock: Arc<dyn Clock>,
    machine_id: String,
}

fn refusal(code: ProtocolErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError {
        code,
        message: message.into(),
        supported: None,
    }
}

fn parse_join(bytes: &[u8]) -> Option<Join> {
    match decode::<HubToBroker>(bytes) {
        Ok(HubToBroker::Join(j)) => Some(j),
        _ => match decode::<RunnerToBroker>(bytes) {
            Ok(RunnerToBroker::Join(j)) => Some(j),
            _ => None,
        },
    }
}

fn reply(role: Role, answer: Result<Joined, ProtocolError>) -> Option<Frame> {
    match role {
        Role::Runner => Frame::json(&match answer {
            Ok(j) => BrokerToRunner::Joined(j),
            Err(e) => BrokerToRunner::Error(e),
        }),
        _ => Frame::json(&match answer {
            Ok(j) => BrokerToHub::Joined(j),
            Err(e) => BrokerToHub::Error(e),
        }),
    }
    .ok()
}

impl JoinDesk {
    /// The desk of the broker with this MachineId.
    pub fn new(
        registry: Arc<Registry>,
        store: Arc<dyn BrokerStore>,
        clock: Arc<dyn Clock>,
        machine_id: String,
    ) -> JoinDesk {
        JoinDesk {
            registry,
            store,
            clock,
            machine_id,
        }
    }

    async fn code_name(&self, code: &str) -> Option<String> {
        let hash = JoinCode::parse(code).ok()?.hash();
        let codes = self.store.join_codes().await.ok()?;
        codes.into_iter().find(|c| c.code_hash == hash)?.name
    }

    async fn redeem(&self, join: Join, key: &str) -> Result<Joined, ProtocolError> {
        let version = negotiate(join.version).map_err(|r| r.into_error())?;
        let kind = match join.role {
            Role::Runner => MachineKind::Runner,
            _ => MachineKind::Hub,
        };
        let name = match kind {
            MachineKind::Hub => self.code_name(&join.code).await.unwrap_or(join.name),
            _ => join.name,
        };
        let req = JoinRequest {
            code: join.code,
            kind,
            name,
            machine_id: join.machine_id,
            public_key: join.public_key,
        };
        let row = self
            .registry
            .redeem(req, key, self.clock.now())
            .await
            .map_err(|e| e.to_protocol())?;
        Ok(Joined {
            version,
            zone: None,
            heartbeat_s: HEARTBEAT_INTERVAL_S,
            broker_lost_kill_s: None,
            key_id: row.public_key,
            broker_machine_id: self.machine_id.clone(),
        })
    }

    /// Serves one joining connection.
    pub async fn serve(&self, t: Box<dyn Transport>) {
        let key = t.peer().key.clone();
        let first = tokio::time::timeout(JOIN_WITHIN, t.recv()).await;
        let (role, answer) = match (key, first) {
            (Some(key), Ok(Ok(frame))) => match parse_join(frame.as_bytes()) {
                Some(join) => (join.role, self.redeem(join, &key).await),
                None => (
                    Role::Hub,
                    Err(refusal(
                        ProtocolErrorCode::Unauthorized,
                        "a connection without a key may only join",
                    )),
                ),
            },
            _ => {
                t.close().await;
                return;
            }
        };
        if let Some(frame) = reply(role, answer) {
            let _ = t.send(frame).await;
        }
        t.close().await;
    }
}

/// A listener that hands joining connections to the [`JoinDesk`] and passes the rest on.
pub struct JoiningListener {
    inner: Arc<dyn Listener>,
    desk: Arc<JoinDesk>,
}

impl JoiningListener {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn Listener>, desk: Arc<JoinDesk>) -> JoiningListener {
        JoiningListener { inner, desk }
    }
}

#[async_trait]
impl Listener for JoiningListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        loop {
            let t = self.inner.accept().await?;
            if t.peer().access != PeerAccess::Joining {
                return Ok(t);
            }
            let desk = self.desk.clone();
            tokio::spawn(async move { desk.serve(t).await });
        }
    }
}

/// What a joiner sends besides the code: its role, name, MachineId and capabilities.
pub struct Joiner {
    /// Hub or runner.
    pub role: Role,
    /// Runner name or hub name.
    pub name: String,
    /// MachineId of the installation.
    pub machine_id: String,
    /// Capabilities of a runner.
    pub capabilities: Option<lr_proto::WireCapabilities>,
}

/// Joins the broker at `address` with `code`: TLS pinned to the fingerprint inside the code,
/// the key proven at the handshake, then `join`; the answer is `joined` or the refusal.
pub async fn join(
    address: &str,
    code: &str,
    key: Arc<ClientKey>,
    who: Joiner,
) -> Result<(Joined, String), String> {
    let parsed = JoinCode::parse(code).map_err(|e| e.to_string())?;
    let fingerprint = parsed.fingerprint().to_string();
    let public_key = key.public_key();
    let t = TlsDialer::new(key)
        .dial(address, Some(&fingerprint))
        .await
        .map_err(|e| format!("{address}: {e}"))?;
    let join = Join {
        version: PROTOCOL_VERSION,
        role: who.role,
        code: code.trim().to_string(),
        public_key,
        name: who.name,
        machine_id: who.machine_id,
        capabilities: who.capabilities,
    };
    let frame = match who.role {
        Role::Runner => Frame::json(&RunnerToBroker::Join(join)),
        _ => Frame::json(&HubToBroker::Join(join)),
    }
    .map_err(|e| e.to_string())?;
    t.send(frame).await.map_err(|e| e.to_string())?;
    let answer = tokio::time::timeout(JOIN_WITHIN, t.recv())
        .await
        .map_err(|_| "the broker did not answer the join in time".to_string())?
        .map_err(|e| e.to_string())?;
    t.close().await;
    let outcome = match who.role {
        Role::Runner => match decode::<BrokerToRunner>(answer.as_bytes()) {
            Ok(BrokerToRunner::Joined(j)) => Ok(j),
            Ok(BrokerToRunner::Error(e)) => Err(e),
            _ => Err(refusal(ProtocolErrorCode::Malformed, "unexpected answer")),
        },
        _ => match decode::<BrokerToHub>(answer.as_bytes()) {
            Ok(BrokerToHub::Joined(j)) => Ok(j),
            Ok(BrokerToHub::Error(e)) => Err(e),
            _ => Err(refusal(ProtocolErrorCode::Malformed, "unexpected answer")),
        },
    };
    outcome
        .map(|j| (j, fingerprint))
        .map_err(|e| format!("the broker refused the join ({:?}): {}", e.code, e.message))
}
