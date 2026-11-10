use crate::auth::{Admit, KeyGate};
use crate::codes::{CodeError, JoinCode};
use crate::error::NetError;
use crate::fingerprint::Fingerprint;
use crate::keys::canonical_public_key;
use async_trait::async_trait;
use lr_abstractions::{
    BrokerStore, Clock, Join, JoinCodeRow, MachineKind, MachineRow, Revoked, StoreError,
};
use lr_core::Timestamp;
use lr_proto::{ProtocolError, ProtocolErrorCode};
use std::fmt;
use std::sync::Arc;

/// Longest machine name.
pub const MAX_NAME_LEN: usize = 64;

/// What `broker add` asks for (spec 3.8, 11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRequest {
    /// Who may join: a hub or runners.
    pub kind: MachineKind,
    /// `--name`: the only name the code admits.
    pub name: Option<String>,
    /// `--label`: the label the joining key gets.
    pub label: Option<String>,
    /// `--runners N`: number of joins, at least 1.
    pub uses: u32,
    /// `[join] code_ttl_min`, at least 1.
    pub ttl_min: u64,
}

/// A join as the joiner presents it in the `join` message. `Debug` hides the code.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// The code string.
    pub code: String,
    /// Hub or runner.
    pub kind: MachineKind,
    /// Name of the joining machine (runner or hub name).
    pub name: String,
    /// MachineId of the joiner.
    pub machine_id: String,
    /// The public key in the message.
    pub public_key: String,
}

impl fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinRequest")
            .field("code", &"<redacted>")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("machine_id", &self.machine_id)
            .field("public_key", &self.public_key)
            .finish()
    }
}

/// Why the registry refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The code string is not a valid code.
    Code(CodeError),
    /// The code was issued by another broker (its fingerprint is not ours).
    ForeignCode,
    /// The key in the `join` message is not the key proven on the connection.
    KeyNotProven,
    /// The MachineId of the joiner is the broker's own (spec 3.8).
    MachineIdClash,
    /// A name, label, count or lifetime is not acceptable.
    Invalid(String),
    /// The store refused: unknown, expired, used up or mismatching code, a taken name.
    Store(StoreError),
}

impl RegistryError {
    /// The protocol refusal sent to the joiner.
    pub fn to_protocol(&self) -> ProtocolError {
        let code = match self {
            RegistryError::Code(_) | RegistryError::ForeignCode => {
                ProtocolErrorCode::JoinCodeUnknown
            }
            RegistryError::KeyNotProven => ProtocolErrorCode::Unauthorized,
            RegistryError::MachineIdClash => ProtocolErrorCode::MachineIdClash,
            RegistryError::Invalid(_) => ProtocolErrorCode::Malformed,
            RegistryError::Store(e) => match e {
                StoreError::CodeUnknown => ProtocolErrorCode::JoinCodeUnknown,
                StoreError::CodeExpired => ProtocolErrorCode::JoinCodeExpired,
                StoreError::CodeUsedUp => ProtocolErrorCode::JoinCodeUsedUp,
                StoreError::CodeKindMismatch => ProtocolErrorCode::JoinCodeKindMismatch,
                StoreError::CodeNameMismatch => ProtocolErrorCode::JoinCodeNameMismatch,
                StoreError::Duplicate(_) => ProtocolErrorCode::NameTaken,
                _ => ProtocolErrorCode::Other,
            },
        };
        ProtocolError {
            code,
            message: self.to_string(),
            supported: None,
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Code(e) => e.fmt(f),
            RegistryError::ForeignCode => f.write_str("the join code is for another broker"),
            RegistryError::KeyNotProven => {
                f.write_str("the joining key is not the key proven on this connection")
            }
            RegistryError::MachineIdClash => {
                f.write_str("the joining MachineId is the broker's own; give the installation its own state directory")
            }
            RegistryError::Invalid(why) => f.write_str(why),
            RegistryError::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<StoreError> for RegistryError {
    fn from(e: StoreError) -> Self {
        RegistryError::Store(e)
    }
}

/// Checks a machine name: 1 to [`MAX_NAME_LEN`] ASCII letters, digits, `.`, `_` or `-`. The
/// colon is reserved for local client keys (`local:<user>`), so the two never collide.
pub fn check_name(name: &str) -> Result<(), RegistryError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(RegistryError::Invalid(format!(
            "machine name `{name}` must be 1-{MAX_NAME_LEN} ASCII letters, digits, `.`, `_` or `-`"
        )))
    }
}

/// The broker's machine registry (spec 3.8, 13.2): issues join codes, redeems them into key
/// records, revokes keys, and as a [`KeyGate`] refuses revoked keys at the handshake and lets
/// an unknown key in to join only while a join code is open.
pub struct Registry {
    store: Arc<dyn BrokerStore>,
    clock: Arc<dyn Clock>,
    own_machine_id: String,
    fingerprint: Fingerprint,
}

impl Registry {
    /// The registry of the broker with this MachineId and certificate fingerprint.
    pub fn new(
        store: Arc<dyn BrokerStore>,
        clock: Arc<dyn Clock>,
        own_machine_id: impl Into<String>,
        fingerprint: Fingerprint,
    ) -> Registry {
        Registry {
            store,
            clock,
            own_machine_id: own_machine_id.into(),
            fingerprint,
        }
    }

    /// Issues a code valid from `now` for `ttl_min` minutes and `uses` joins; only its hash
    /// is stored. The returned code is shown once and never stored.
    pub async fn issue(&self, req: CodeRequest, now: Timestamp) -> Result<JoinCode, RegistryError> {
        if req.uses == 0 {
            return Err(RegistryError::Invalid(
                "a code needs at least one use".into(),
            ));
        }
        if req.ttl_min == 0 {
            return Err(RegistryError::Invalid(
                "code_ttl_min must be at least 1".into(),
            ));
        }
        if let Some(name) = &req.name {
            check_name(name)?;
        }
        let code = JoinCode::generate(self.fingerprint)
            .map_err(|e: NetError| RegistryError::Invalid(e.to_string()))?;
        self.store
            .issue_code(JoinCodeRow {
                code_hash: code.hash(),
                kind: req.kind,
                name: req.name,
                label: req.label,
                uses_left: req.uses,
                issued_at: now,
                expires_at: now.plus_secs(req.ttl_min.saturating_mul(60)),
                redeemed_at: None,
                machine_name: None,
            })
            .await?;
        Ok(code)
    }

    /// Redeems a join presented on a connection whose proven key is `proven_key`: checks the
    /// code is ours, the key is the proven one, the MachineId is not the broker's, then records
    /// the key and spends one use in one store write (spec 3.8).
    pub async fn redeem(
        &self,
        req: JoinRequest,
        proven_key: &str,
        now: Timestamp,
    ) -> Result<MachineRow, RegistryError> {
        let code = JoinCode::parse(&req.code).map_err(RegistryError::Code)?;
        if code.fingerprint() != self.fingerprint {
            return Err(RegistryError::ForeignCode);
        }
        let claimed =
            canonical_public_key(&req.public_key).map_err(|_| RegistryError::KeyNotProven)?;
        let proven = canonical_public_key(proven_key).map_err(|_| RegistryError::KeyNotProven)?;
        if claimed != proven {
            return Err(RegistryError::KeyNotProven);
        }
        if req.machine_id == self.own_machine_id {
            return Err(RegistryError::MachineIdClash);
        }
        if req.machine_id.trim().is_empty() {
            return Err(RegistryError::Invalid("MachineId is empty".into()));
        }
        check_name(&req.name)?;
        let machine = MachineRow {
            name: req.name,
            machine_id: req.machine_id,
            kind: req.kind,
            label: String::new(),
            public_key: claimed,
            registered_at: now,
            last_seen: Some(now),
            revoked_at: None,
        };
        Ok(self
            .store
            .redeem(Join {
                code_hash: code.hash(),
                at: now,
                machine,
            })
            .await?)
    }

    /// Revokes the key of machine `name` at once (`runner remove`, `client remove`).
    pub async fn revoke(&self, name: &str, now: Timestamp) -> Result<Revoked, RegistryError> {
        Ok(self.store.revoke(name, now).await?)
    }

    /// All key records with their labels, revoked ones included.
    pub async fn machines(&self) -> Result<Vec<MachineRow>, RegistryError> {
        Ok(self.store.machines().await?)
    }

    /// Whether the network listener must be up (spec 3.8, 17): a code with uses left that has
    /// not expired, or at least one unrevoked machine.
    pub async fn should_listen(&self, now: Timestamp) -> Result<bool, RegistryError> {
        if self.code_open(now).await? {
            return Ok(true);
        }
        let machines = self.store.machines().await?;
        Ok(machines.iter().any(|m| m.revoked_at.is_none()))
    }

    /// Whether a join code with uses left has not expired at `now`.
    pub async fn code_open(&self, now: Timestamp) -> Result<bool, RegistryError> {
        let codes = self.store.join_codes().await?;
        Ok(codes.iter().any(|c| c.uses_left > 0 && now < c.expires_at))
    }
}

fn storage_refusal(e: impl fmt::Display) -> Admit {
    Admit::Refused(ProtocolError {
        code: ProtocolErrorCode::Other,
        message: format!("broker storage: {e}"),
        supported: None,
    })
}

#[async_trait]
impl KeyGate for Registry {
    async fn admit(&self, public_key: &str) -> Admit {
        let Ok(public_key) = canonical_public_key(public_key) else {
            return Admit::Refused(ProtocolError {
                code: ProtocolErrorCode::Unauthorized,
                message: "not an ed25519 public key".into(),
                supported: None,
            });
        };
        match self.store.machine_by_key(&public_key).await {
            Ok(Some(m)) if m.revoked_at.is_some() => Admit::Refused(ProtocolError {
                code: ProtocolErrorCode::KeyRevoked,
                message: format!("the key of `{}` was revoked", m.name),
                supported: None,
            }),
            Ok(Some(_)) => Admit::Member,
            Ok(None) => match self.code_open(self.clock.now()).await {
                Ok(true) => Admit::Joining,
                Ok(false) => Admit::Refused(ProtocolError {
                    code: ProtocolErrorCode::JoinCodeUnknown,
                    message: "unknown key and no join code is open on this broker".into(),
                    supported: None,
                }),
                Err(e) => storage_refusal(e),
            },
            Err(e) => storage_refusal(e),
        }
    }
}
