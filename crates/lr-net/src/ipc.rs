use crate::error::NetError;
use crate::stream::StreamTransport;
use async_trait::async_trait;
use lr_abstractions::{Dialer, Listener, Transport, TransportError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

#[cfg(unix)]
use crate::ipc_unix as platform;
#[cfg(windows)]
use crate::ipc_windows as platform;

/// Bytes every IPC client writes first, after it verified the server. On Windows the server can
/// ask the kernel who the client is only after reading from the pipe; the preamble also
/// refuses a stray program that is not a LocalRunner client.
pub const PREAMBLE: &[u8; 8] = b"LRIPC/1\n";

/// Time a new IPC client has to send the preamble.
pub const PREAMBLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Who may open the local channel (spec 10, table "IPC").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcAccess {
    /// Only the OS user that created it (hub, runner control): Windows DACL with the user's SID
    /// alone, Unix socket mode `0600`.
    OwnerOnly,
    /// The broker service: these accounts (the `[[local_user]]` names) plus the owner, SYSTEM
    /// and Administrators. Windows: a protected DACL with the SID of each account, which may
    /// read and write but not create pipe instances; a bare name is a local account of this
    /// computer, and a SID (`S-1-...`) may be given instead of a name (it matches in
    /// [`crate::LocalUsers`] too). Unix: socket mode `0660`; the installer gives the socket the
    /// `al-localrunner` group, whose members are the listed users (the names are not used by
    /// the socket).
    Principals(Vec<String>),
    /// The broker's administrative channel (spec 3.9): the owner (the service account of a
    /// broker service) and the machine's administrators. Windows: a protected DACL with the
    /// owner, SYSTEM and Administrators; Unix: socket mode `0600` (root passes anyway).
    OwnerAndAdmins,
}

/// Whom an IPC client accepts as the server, checked BEFORE it sends anything (a pipe or
/// socket name can be taken by another user first). Windows: the owner SID of the pipe
/// object; Unix: the uid of the socket's peer (`SO_PEERCRED`). A server that cannot be
/// identified is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    principals: Vec<String>,
}

impl ServerIdentity {
    /// The current OS user: hub and runner control channels, and a broker run by the same
    /// user.
    pub fn current_user() -> Result<ServerIdentity, NetError> {
        Ok(ServerIdentity::principal(platform::current_principal()?))
    }

    /// One principal: a SID (`S-1-5-...`) on Windows, a numeric uid on Unix.
    pub fn principal(id: impl Into<String>) -> ServerIdentity {
        ServerIdentity {
            principals: vec![id.into()],
        }
    }

    /// Also accepts `id`.
    pub fn or(mut self, id: impl Into<String>) -> ServerIdentity {
        self.principals.push(id.into());
        self
    }

    /// Also accepts the machine's administrators: SYSTEM and Administrators on Windows, root
    /// on Unix (a broker service installed by an administrator).
    pub fn or_admins(self) -> ServerIdentity {
        platform::admin_principals()
            .iter()
            .fold(self, |acc, id| acc.or(*id))
    }

    /// Whether `id` is one of the accepted principals (SIDs compare case-insensitively).
    pub fn accepts(&self, id: &str) -> bool {
        self.principals.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    pub(crate) fn check(&self, actual: &str) -> Result<(), TransportError> {
        if self.accepts(actual) {
            Ok(())
        } else {
            Err(TransportError::Refused(format!(
                "untrusted_server: the IPC endpoint belongs to {actual}, expected one of {}; \
                 nothing was sent",
                self.principals.join(", ")
            )))
        }
    }
}

/// The OS user of this process, as the kernel reports it (the same form as IPC peers:
/// `DOMAIN\name` on Windows, the login name on Unix).
pub fn current_os_user() -> Result<String, NetError> {
    platform::current_user()
}

/// The security principal of this process: the SID (`S-1-5-...`) on Windows, the numeric uid
/// on Unix; the form [`ServerIdentity::principal`] takes.
pub fn current_os_principal() -> Result<String, NetError> {
    platform::current_principal()
}

pub(crate) fn account_of_sid(sid: &str) -> Option<String> {
    platform::account_of_sid(sid)
}

/// The principal of account `name` in the form [`ServerIdentity::principal`] takes: the SID on
/// Windows (a bare name is a local account, `NT SERVICE\name` a service account, a SID passes
/// through), the numeric uid on Unix. An unknown account is an error.
pub fn account_principal(name: &str) -> Result<String, NetError> {
    platform::account_principal(name)
}

/// Whether OS user `os_user` (as the kernel reports an IPC peer) administers this machine
/// (spec 3.5, 3.9): on Windows SYSTEM or a member of the local Administrators group, on Unix
/// root. Looks the account up, so call it off the async runtime.
pub fn is_machine_admin(os_user: &str) -> bool {
    platform::is_machine_admin(os_user)
}

/// The computer name that qualifies a bare `[[local_user]]` name on Windows; empty elsewhere.
pub fn local_machine_name() -> String {
    platform::machine_name()
}

pub(crate) async fn read_preamble<R: AsyncRead + Unpin>(r: &mut R) -> bool {
    let mut got = [0u8; PREAMBLE.len()];
    matches!(
        tokio::time::timeout(PREAMBLE_TIMEOUT, r.read_exact(&mut got)).await,
        Ok(Ok(_))
    ) && &got == PREAMBLE
}

/// The local IPC listener: a named pipe on Windows, a Unix domain socket elsewhere. Every
/// accepted connection carries the peer's OS user as reported by the kernel (the named-pipe
/// client token on Windows, `SO_PEERCRED`/`getpeereid` on Unix) and no key.
pub struct IpcListener {
    ready: Mutex<mpsc::Receiver<StreamTransport>>,
    task: JoinHandle<()>,
    access: platform::AccessCell,
    #[cfg_attr(not(unix), allow(dead_code))]
    path: String,
    closed: AtomicBool,
}

impl IpcListener {
    /// Creates the pipe (`\\.\pipe\<name>`) or socket path and starts accepting. On Windows
    /// the pipe must not exist yet, so another user cannot have created it first.
    pub async fn bind(address: &str, access: IpcAccess) -> Result<IpcListener, NetError> {
        let (tx, rx) = mpsc::channel(16);
        let (task, cell) = platform::listen(address, &access, tx).await?;
        Ok(IpcListener {
            ready: Mutex::new(rx),
            task,
            access: cell,
            path: address.to_string(),
            closed: AtomicBool::new(false),
        })
    }

    /// Changes who may connect from now on (`policy reload` changed `[[local_user]]`): new
    /// pipe instances get the new DACL; connections already open stay.
    pub async fn set_access(&self, access: IpcAccess) -> Result<(), NetError> {
        platform::update_access(&self.access, &access).await
    }
}

impl IpcListener {
    /// Stops accepting at once and, on Unix, removes the socket file, even while something
    /// else still holds this listener: a stopped role no longer answers on its address.
    /// Connections already accepted stay.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        self.close();
    }
}

#[async_trait]
impl Listener for IpcListener {
    async fn accept(&self) -> Result<Box<dyn Transport>, TransportError> {
        let mut ready = self.ready.lock().await;
        match ready.recv().await {
            Some(t) => Ok(Box::new(t)),
            None => Err(TransportError::Closed),
        }
    }
}

/// Dials a local IPC endpoint after checking that its server is the expected
/// [`ServerIdentity`]; the fingerprint must be `None`. The peer is the server's OS user.
#[derive(Debug, Clone)]
pub struct IpcDialer {
    server: ServerIdentity,
}

impl IpcDialer {
    /// A dialer that talks only to servers run by `server`.
    pub fn new(server: ServerIdentity) -> IpcDialer {
        IpcDialer { server }
    }
}

#[async_trait]
impl Dialer for IpcDialer {
    async fn dial(
        &self,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Box<dyn Transport>, TransportError> {
        if fingerprint.is_some() {
            return Err(TransportError::Refused(
                "local IPC takes no certificate fingerprint".into(),
            ));
        }
        Ok(Box::new(platform::dial(address, &self.server).await?))
    }
}
