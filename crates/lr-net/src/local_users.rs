use lr_abstractions::Peer;
use lr_core::Endpoint;
use lr_proto::{PeerKind, RejectReason};

/// Prefix of the client key of a local user: `local:<os user>`. Machine names cannot contain a
/// colon, so a local client never shares a key with a joined one.
pub const LOCAL_CLIENT_PREFIX: &str = "local:";

/// One `[[local_user]]` entry of `policy.toml` (spec 3.8): an OS user allowed on the broker's
/// local IPC, with the label its orders get. On Windows `name` is `DOMAIN\name`, or a bare
/// name meaning a local account of this computer (`COMPUTER\name`); on Unix the login name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalUser {
    /// OS account name.
    pub name: String,
    /// Label of the client record.
    pub label: String,
}

/// The client record of an admitted local IPC peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalClient {
    /// OS user reported by the kernel (`DOMAIN\name` on Windows).
    pub os_user: String,
    /// Client key: [`LOCAL_CLIENT_PREFIX`] and the qualified OS user; idempotency and cancel
    /// rights of two users of one machine never overlap, nor those of a domain and a local
    /// account of the same short name.
    pub client_key: String,
    /// Label from the entry.
    pub label: String,
}

/// The `[[local_user]]` list: who may use the broker over local IPC without a key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalUsers {
    users: Vec<LocalUser>,
    matches: Vec<Option<String>>,
}

fn is_sid(name: &str) -> bool {
    cfg!(windows) && (name.starts_with("S-1-") || name.starts_with("s-1-"))
}

impl LocalUsers {
    /// The list from the policy; bare Windows names are qualified with this computer's name.
    /// Looks up the accounts of SID entries, so call it off the async runtime.
    pub fn new(users: Vec<LocalUser>) -> LocalUsers {
        LocalUsers::with_machine(users, crate::ipc::local_machine_name())
    }

    /// The list with an explicit computer name for bare Windows names. A Windows entry may be a
    /// SID (`S-1-5-21-...`): it matches the account that SID belongs to, resolved here; a SID
    /// that names no account matches nobody.
    pub fn with_machine(users: Vec<LocalUser>, machine: impl Into<String>) -> LocalUsers {
        let machine = machine.into();
        let matches = users.iter().map(|u| qualified(&u.name, &machine)).collect();
        LocalUsers { users, matches }
    }

    /// The client record of `peer`. Only a local IPC peer whose kernel-reported OS user is in
    /// the list is admitted; anyone else gets `rejected_local_user_unknown`. On Windows the
    /// qualified names compare case-insensitively (account names are), so `CORP\alex` and
    /// `PC\alex` are different users; elsewhere names compare exactly.
    pub fn admit(&self, peer: &Peer) -> Result<LocalClient, RejectReason> {
        let Endpoint::Ipc { os_user } = &peer.endpoint else {
            return Err(RejectReason::LocalUserUnknown);
        };
        let user = self
            .users
            .iter()
            .zip(&self.matches)
            .find(|(_, m)| m.as_deref().is_some_and(|m| same_user(m, os_user)))
            .map(|(u, _)| u)
            .ok_or(RejectReason::LocalUserUnknown)?;
        Ok(LocalClient {
            os_user: os_user.clone(),
            client_key: format!("{LOCAL_CLIENT_PREFIX}{os_user}"),
            label: user.label.clone(),
        })
    }
}

fn qualified(entry: &str, machine: &str) -> Option<String> {
    if !cfg!(windows) {
        return Some(entry.to_string());
    }
    if is_sid(entry) {
        return crate::ipc::account_of_sid(entry);
    }
    Some(if entry.contains('\\') {
        entry.to_string()
    } else {
        format!("{machine}\\{entry}")
    })
}

fn same_user(entry: &str, os_user: &str) -> bool {
    if cfg!(windows) {
        entry.to_lowercase() == os_user.to_lowercase()
    } else {
        entry == os_user
    }
}

/// How the protocol must treat a peer: local IPC (admin commands and local-only channels
/// allowed) or network (refused, spec 3.9). Taken from how the transport connected it, never
/// from anything the peer says.
pub fn peer_kind(peer: &Peer) -> PeerKind {
    match peer.endpoint {
        Endpoint::Ipc { .. } => PeerKind::Ipc,
        Endpoint::Addr(_) => PeerKind::Network,
    }
}
