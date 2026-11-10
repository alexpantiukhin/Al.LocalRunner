use crate::error::NetError;
use crate::ipc::{read_preamble, IpcAccess, PREAMBLE};
use crate::os_windows::{process_token, token_sid_string, token_user_name, Descriptor, Token};
use crate::stream::StreamTransport;
use lr_abstractions::{Peer, TransportError};
use lr_core::Endpoint;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

const CLIENT_ACCESS: u32 = 0x0012_018B;
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn current_user() -> Result<String, NetError> {
    token_user_name(&process_token()?)
}

fn sddl(access: IpcAccess) -> Result<String, NetError> {
    let owner = token_sid_string(&process_token()?)?;
    Ok(match access {
        IpcAccess::OwnerOnly => format!("D:P(A;;GA;;;{owner})"),
        IpcAccess::LocalUsers => {
            format!("D:P(A;;GA;;;{owner})(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x{CLIENT_ACCESS:08X};;;AU)")
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
            .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void)
    };
    server.map_err(|e| NetError::Io(format!("named pipe {name}: {e}")))
}

pub(crate) async fn listen(
    name: &str,
    access: IpcAccess,
    tx: mpsc::Sender<StreamTransport>,
) -> Result<JoinHandle<()>, NetError> {
    let sddl = sddl(access)?;
    let mut server = create(name, &sddl, true)?;
    let name = name.to_string();
    Ok(tokio::spawn(async move {
        loop {
            let connected = server.connect().await;
            let next = loop {
                match create(&name, &sddl, false) {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            let pipe = std::mem::replace(&mut server, next);
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
    }))
}

async fn handshake(mut pipe: NamedPipeServer) -> Option<StreamTransport> {
    if !read_preamble(&mut pipe).await {
        return None;
    }
    let os_user = client_user(&pipe).ok()?;
    let peer = Peer {
        endpoint: Endpoint::Ipc { os_user },
        key: None,
    };
    Some(StreamTransport::new(pipe, peer))
}

fn client_user(pipe: &NamedPipeServer) -> Result<String, NetError> {
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
    token_user_name(&Token(token)).map_err(|e| NetError::PeerUser(e.to_string()))
}

fn open_client(name: &str) -> std::io::Result<NamedPipeClient> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            CLIENT_ACCESS,
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

fn server_user(pipe: &NamedPipeClient) -> String {
    let lookup = || -> Option<String> {
        let mut pid = 0u32;
        if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) } == 0 {
            return None;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let mut token: HANDLE = null_mut();
        let ok = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return None;
        }
        token_user_name(&Token(token)).ok()
    };
    lookup().unwrap_or_else(|| "unknown".to_string())
}

pub(crate) async fn dial(name: &str) -> Result<StreamTransport, TransportError> {
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
    pipe.write_all(PREAMBLE)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let peer = Peer {
        endpoint: Endpoint::Ipc {
            os_user: server_user(&pipe),
        },
        key: None,
    };
    Ok(StreamTransport::new(pipe, peer))
}
