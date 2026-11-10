use crate::error::NetError;
use crate::ipc::{read_preamble, IpcAccess, PREAMBLE};
use crate::os_unix::user_name;
use crate::stream::StreamTransport;
use lr_abstractions::{Peer, TransportError};
use lr_core::Endpoint;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(crate) fn current_user() -> Result<String, NetError> {
    crate::os_unix::current_user()
}

async fn clear_stale(path: &Path) -> Result<(), NetError> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.file_type().is_socket() {
        return Err(NetError::Io(format!(
            "{} exists and is not a socket",
            path.display()
        )));
    }
    if UnixStream::connect(path).await.is_ok() {
        return Err(NetError::Io(format!("{} is in use", path.display())));
    }
    std::fs::remove_file(path)?;
    Ok(())
}

pub(crate) async fn listen(
    address: &str,
    access: IpcAccess,
    tx: mpsc::Sender<StreamTransport>,
) -> Result<JoinHandle<()>, NetError> {
    let path = Path::new(address);
    clear_stale(path).await?;
    let listener = UnixListener::bind(path)?;
    let mode = match access {
        IpcAccess::OwnerOnly => 0o600,
        IpcAccess::LocalUsers => 0o660,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(tokio::spawn(async move {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Some(t) = handshake(stream).await {
                    let _ = tx.send(t).await;
                }
            });
        }
    }))
}

async fn handshake(mut stream: UnixStream) -> Option<StreamTransport> {
    let uid = stream.peer_cred().ok()?.uid();
    if !read_preamble(&mut stream).await {
        return None;
    }
    let peer = Peer {
        endpoint: Endpoint::Ipc {
            os_user: user_name(uid),
        },
        key: None,
    };
    Some(StreamTransport::new(stream, peer))
}

pub(crate) async fn dial(address: &str) -> Result<StreamTransport, TransportError> {
    let mut stream = UnixStream::connect(address)
        .await
        .map_err(|e| TransportError::Unreachable(format!("{address}: {e}")))?;
    let os_user = stream
        .peer_cred()
        .map(|c| user_name(c.uid()))
        .unwrap_or_else(|_| "unknown".to_string());
    stream
        .write_all(PREAMBLE)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
    };
    Ok(StreamTransport::new(stream, peer))
}
