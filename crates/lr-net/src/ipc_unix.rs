use crate::error::NetError;
use crate::ipc::{read_preamble, IpcAccess, ServerIdentity, PREAMBLE};
use crate::os_unix::user_name;
use crate::stream::StreamTransport;
use lr_abstractions::{Peer, PeerAccess, TransportError};
use lr_core::Endpoint;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(crate) type AccessCell = PathBuf;

pub(crate) fn current_user() -> Result<String, NetError> {
    crate::os_unix::current_user()
}

pub(crate) fn current_principal() -> Result<String, NetError> {
    Ok(unsafe { libc::geteuid() }.to_string())
}

pub(crate) fn admin_principals() -> &'static [&'static str] {
    &["0"]
}

pub(crate) fn account_of_sid(_sid: &str) -> Option<String> {
    None
}

pub(crate) fn is_machine_admin(os_user: &str) -> bool {
    crate::os_unix::uid_of(os_user) == Some(0)
}

pub(crate) fn account_principal(name: &str) -> Result<String, NetError> {
    crate::os_unix::uid_of(name)
        .map(|uid| uid.to_string())
        .ok_or_else(|| NetError::Io(format!("unknown account `{name}`")))
}

pub(crate) fn machine_name() -> String {
    String::new()
}

fn mode(access: &IpcAccess) -> u32 {
    match access {
        IpcAccess::OwnerOnly | IpcAccess::OwnerAndAdmins => 0o600,
        IpcAccess::Principals(_) => 0o660,
    }
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

fn bind_private(path: &Path, access: &IpcAccess) -> Result<UnixListener, NetError> {
    let mut staging = path.as_os_str().to_owned();
    staging.push(format!(".bind-{}", std::process::id()));
    let staging = PathBuf::from(staging);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let inner = staging.join("s");
    let bound = (|| {
        let listener = UnixListener::bind(&inner)?;
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(mode(access)))?;
        std::fs::rename(&inner, path)?;
        Ok::<_, std::io::Error>(listener)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    Ok(bound?)
}

pub(crate) async fn listen(
    address: &str,
    access: &IpcAccess,
    tx: mpsc::Sender<StreamTransport>,
) -> Result<(JoinHandle<()>, AccessCell), NetError> {
    let path = Path::new(address);
    clear_stale(path).await?;
    let listener = bind_private(path, access)?;
    let task = tokio::spawn(async move {
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
    });
    Ok((task, path.to_path_buf()))
}

pub(crate) async fn update_access(cell: &AccessCell, access: &IpcAccess) -> Result<(), NetError> {
    std::fs::set_permissions(cell, std::fs::Permissions::from_mode(mode(access)))?;
    Ok(())
}

async fn handshake(mut stream: UnixStream) -> Option<StreamTransport> {
    let uid = stream.peer_cred().ok()?.uid();
    if !read_preamble(&mut stream).await {
        return None;
    }
    let os_user = tokio::task::spawn_blocking(move || user_name(uid))
        .await
        .ok()?;
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
        access: PeerAccess::Local,
    };
    Some(StreamTransport::new(stream, peer))
}

pub(crate) async fn dial(
    address: &str,
    server: &ServerIdentity,
) -> Result<StreamTransport, TransportError> {
    let mut stream = UnixStream::connect(address)
        .await
        .map_err(|e| TransportError::Unreachable(format!("{address}: {e}")))?;
    let uid = stream
        .peer_cred()
        .map_err(|e| {
            TransportError::Refused(format!(
                "untrusted_server: cannot tell who serves {address}: {e}; nothing was sent"
            ))
        })?
        .uid();
    server.check(&uid.to_string())?;
    stream
        .write_all(PREAMBLE)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let os_user = tokio::task::spawn_blocking(move || user_name(uid))
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
        access: PeerAccess::Local,
    };
    Ok(StreamTransport::new(stream, peer))
}
