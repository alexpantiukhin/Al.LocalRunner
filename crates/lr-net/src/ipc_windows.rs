use crate::acl_windows::{
    handle_owner, pipe_sddl, set_handle_dacl, Descriptor, PIPE_CLIENT_ACCESS,
};
use crate::error::NetError;
use crate::ipc::{read_preamble, IpcAccess, ServerIdentity, PREAMBLE};
use crate::os_windows::{
    account_sid, computer_name, process_token, sid_text_account_name, token_sid_string,
    token_user_name, Token, ADMINISTRATORS_SID, SYSTEM_SID,
};
use crate::stream::StreamTransport;
use lr_abstractions::{Peer, PeerAccess, TransportError};
use lr_core::Endpoint;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct AccessState {
    sddl: String,
    waiting: usize,
}

pub(crate) type AccessCell = Arc<Mutex<AccessState>>;

fn lock(cell: &AccessCell) -> MutexGuard<'_, AccessState> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn current_user() -> Result<String, NetError> {
    token_user_name(&process_token()?)
}

pub(crate) fn current_principal() -> Result<String, NetError> {
    token_sid_string(&process_token()?)
}

pub(crate) fn admin_principals() -> &'static [&'static str] {
    &[SYSTEM_SID, ADMINISTRATORS_SID]
}

pub(crate) fn account_of_sid(sid: &str) -> Option<String> {
    sid_text_account_name(sid).ok()
}

pub(crate) fn account_principal(name: &str) -> Result<String, NetError> {
    account_sid(name)
}

pub(crate) fn is_machine_admin(os_user: &str) -> bool {
    crate::service_acl_windows::is_machine_admin(os_user)
}

pub(crate) fn machine_name() -> String {
    computer_name().unwrap_or_default()
}

async fn sddl_off_runtime(access: &IpcAccess) -> Result<String, NetError> {
    let access = access.clone();
    tokio::task::spawn_blocking(move || sddl(&access))
        .await
        .map_err(|e| NetError::Io(e.to_string()))?
}

fn sddl(access: &IpcAccess) -> Result<String, NetError> {
    let owner = current_principal()?;
    Ok(match access {
        IpcAccess::OwnerOnly => pipe_sddl(&owner, None),
        IpcAccess::OwnerAndAdmins => pipe_sddl(&owner, Some(&[])),
        IpcAccess::Principals(names) => {
            let sids = names
                .iter()
                .map(|n| account_sid(n))
                .collect::<Result<Vec<_>, _>>()?;
            pipe_sddl(&owner, Some(&sids))
        }
    })
}

fn create(name: &str, sddl: &str, first: bool) -> Result<NamedPipeServer, NetError> {
    let sd = Descriptor::from_sddl(sddl)?;
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .write_dac(true)
            .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void)
    };
    server.map_err(|e| NetError::Io(format!("named pipe {name}: {e}")))
}

pub(crate) async fn listen(
    name: &str,
    access: &IpcAccess,
    tx: mpsc::Sender<StreamTransport>,
) -> Result<(JoinHandle<()>, AccessCell), NetError> {
    let sddl = sddl_off_runtime(access).await?;
    let mut server = create(name, &sddl, true)?;
    let cell: AccessCell = Arc::new(Mutex::new(AccessState {
        sddl,
        waiting: server.as_raw_handle() as usize,
    }));
    let name = name.to_string();
    let shared = cell.clone();
    let task = tokio::spawn(async move {
        loop {
            let connected = server.connect().await;
            let pipe = loop {
                let created = {
                    let mut state = lock(&shared);
                    create(&name, &state.sddl, false).map(|next| {
                        state.waiting = next.as_raw_handle() as usize;
                        std::mem::replace(&mut server, next)
                    })
                };
                match created {
                    Ok(pipe) => break pipe,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            if connected.is_err() {
                continue;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Some(t) = handshake(pipe).await {
                    let _ = tx.send(t).await;
                }
            });
        }
    });
    Ok((task, cell))
}

pub(crate) async fn update_access(cell: &AccessCell, access: &IpcAccess) -> Result<(), NetError> {
    let new = sddl_off_runtime(access).await?;
    let mut state = lock(cell);
    set_handle_dacl(state.waiting as HANDLE, &new)?;
    state.sddl = new;
    Ok(())
}

async fn handshake(mut pipe: NamedPipeServer) -> Option<StreamTransport> {
    if !read_preamble(&mut pipe).await {
        return None;
    }
    let token = client_token(&pipe).ok()?;
    let os_user = tokio::task::spawn_blocking(move || token_user_name(&token))
        .await
        .ok()?
        .ok()?;
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
        access: PeerAccess::Local,
    };
    Some(StreamTransport::new(pipe, peer))
}

fn client_token(pipe: &NamedPipeServer) -> Result<Token, NetError> {
    let handle = pipe.as_raw_handle() as HANDLE;
    if unsafe { ImpersonateNamedPipeClient(handle) } == 0 {
        return Err(NetError::PeerUser(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let mut token: HANDLE = null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let open_error = std::io::Error::last_os_error();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    if opened == 0 {
        return Err(NetError::PeerUser(open_error.to_string()));
    }
    Ok(Token(token))
}

fn open_client(name: &str) -> std::io::Result<NamedPipeClient> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            PIPE_CLIENT_ACCESS,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    unsafe { NamedPipeClient::from_raw_handle(handle as RawHandle) }
}

/// Owner SID of the pipe a client handle is connected to.
pub(crate) fn pipe_owner(pipe: &NamedPipeClient) -> Result<String, NetError> {
    handle_owner(pipe.as_raw_handle() as HANDLE)
}

pub(crate) async fn dial(
    name: &str,
    server: &ServerIdentity,
) -> Result<StreamTransport, TransportError> {
    let deadline = Instant::now() + DIAL_TIMEOUT;
    let mut pipe = loop {
        match open_client(name) {
            Ok(pipe) => break pipe,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if Instant::now() >= deadline {
                    return Err(TransportError::Unreachable(format!("{name}: busy")));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(TransportError::Unreachable(format!("{name}: {e}"))),
        }
    };
    let owner = pipe_owner(&pipe).map_err(|e| {
        TransportError::Refused(format!(
            "untrusted_server: cannot tell who owns {name}: {e}; nothing was sent"
        ))
    })?;
    server.check(&owner)?;
    pipe.write_all(PREAMBLE)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let sid = owner.clone();
    let os_user = tokio::task::spawn_blocking(move || sid_text_account_name(&sid))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(owner);
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
        access: PeerAccess::Local,
    };
    Ok(StreamTransport::new(pipe, peer))
}
