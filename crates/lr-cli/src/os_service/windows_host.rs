//! The Windows service host of `broker run --service`: talks to the service control manager,
//! gives the process a hidden console (so the soft stop reaches steps of any role it hosts,
//! spec 10) and runs the broker until the manager stops the service.

use super::{service_name, Os};
use crate::paths::Layout;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use windows_sys::core::PWSTR;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};

const NO_ERROR: u32 = 0;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

static LAYOUT: OnceLock<Layout> = OnceLock::new();
static STATUS: AtomicUsize = AtomicUsize::new(0);
static CHECKPOINT: AtomicU32 = AtomicU32::new(0);
static STOP: Mutex<Option<Sender<()>>> = Mutex::new(None);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn report(state: SERVICE_STATUS_CURRENT_STATE, specific: u32, wait_ms: u32) {
    let handle = STATUS.load(Ordering::SeqCst) as *mut c_void;
    if handle.is_null() {
        return;
    }
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: if specific == 0 {
            NO_ERROR
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
        dwServiceSpecificExitCode: specific,
        dwCheckPoint: if pending {
            CHECKPOINT.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            0
        },
        dwWaitHint: wait_ms,
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

unsafe extern "system" fn handler(control: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING, 0, 15_000);
            if let Some(tx) = STOP.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(());
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

async fn serve(layout: &Layout, stop: std::sync::mpsc::Receiver<()>) -> u32 {
    let running = match crate::roles::start_broker(layout, false).await {
        Ok(r) => r,
        Err(_) => return 1,
    };
    report(SERVICE_RUNNING, 0, 0);
    let stopped = tokio::task::spawn_blocking(move || {
        let _ = stop.recv();
    });
    tokio::select! {
        _ = stopped => {}
        _ = running.wait() => {}
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(service_name(Os::Windows));
    let handle = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null());
    if handle.is_null() {
        return;
    }
    STATUS.store(handle as usize, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 0, 15_000);
    let _ = lr_exec::ensure_console();
    let (tx, rx) = channel();
    *STOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    let code = match (
        LAYOUT.get(),
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build(),
    ) {
        (Some(layout), Ok(rt)) => rt.block_on(serve(layout, rx)),
        _ => 1,
    };
    report(SERVICE_STOPPED, code, 0);
}

/// Hands this process to the service control manager and runs the broker of `layout` as the
/// service; returns when the service stopped. Fails when the process was not started by the
/// service manager.
pub fn run(layout: Layout) -> Result<(), String> {
    let _ = LAYOUT.set(layout);
    let mut name = wide(service_name(Os::Windows));
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(format!(
            "--service is for the Windows service manager: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}
