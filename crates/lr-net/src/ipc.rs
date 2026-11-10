use crate::error::NetError;
use crate::stream::StreamTransport;
use async_trait::async_trait;
use lr_abstractions::{Dialer, Listener, Transport, TransportError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

#[cfg(unix)]
use crate::ipc_unix as platform;
#[cfg(windows)]
use crate::ipc_windows as platform;

/// Bytes every IPC client writes first. On Windows the server can ask the kernel who the
/// client is only after reading from the pipe; the preamble also refuses a stray program
/// that is not a LocalRunner client.
pub const PREAMBLE: &[u8; 8] = b"LRIPC/1\n";

/// Time a new IPC client has to send the preamble.
pub const PREAMBLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Who may open the local channel (spec 10, table "IPC").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcAccess {
    /// Only the OS user that created it (hub, runner control): Windows DACL with the user's SID
    /// alone, Unix socket mode `0600`.
    OwnerOnly,
    /// Any local user may connect and the broker admits only `[[local_user]]` entries
    /// (broker service): Windows DACL for authenticated users with read and write but without
    /// the right to create pipe instances, plus the owner, SYSTEM and administrators; Unix
    /// socket mode `0660` (the installer gives the socket the `al-localrunner` group).
    LocalUsers,
}

/// The OS user of this process, as the kernel reports it (the same form as IPC peers).
pub fn current_os_user() -> Result<String, NetError> {
    platform::current_user()
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
    #[cfg_attr(not(unix), allow(dead_code))]
    path: String,
}

impl IpcListener {
    /// Creates the pipe (`\\.\pipe\<name>`) or socket path and starts accepting. On Windows
    /// the pipe must not exist yet, so another user cannot have created it first.
    pub async fn bind(address: &str, access: IpcAccess) -> Result<IpcListener, NetError> {
        let (tx, rx) = mpsc::channel(16);
        let task = platform::listen(address, access, tx).await?;
        Ok(IpcListener {
            ready: Mutex::new(rx),
            task,
            path: address.to_string(),
        })
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
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

/// Dials a local IPC endpoint; the fingerprint must be `None`. The peer is the server's OS
/// user when the OS tells it, else `unknown`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpcDialer;

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
        Ok(Box::new(platform::dial(address).await?))
    }
}
