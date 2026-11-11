//! The Windows service host of `broker run --service`: talks to the service control manager,
//! gives the process a hidden console (so the soft stop reaches steps of any role it hosts,
//! spec 10) and runs the broker until the manager stops the service.

use super::service_run::{self, ServiceEnd};
use super::{service_name, Os};
use crate::paths::Layout;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::watch;
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
static STOP: OnceLock<watch::Sender<bool>> = OnceLock::new();
const BROKER_ENDED: u32 = 2;

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
            if let Some(tx) = STOP.get() {
                tx.send_replace(true);
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

async fn serve(layout: &Layout, mut stop: watch::Receiver<bool>) -> u32 {
    let stopped = async move {
        let _ = stop.wait_for(|s| *s).await;
    };
    match service_run::serve(layout, || report(SERVICE_RUNNING, 0, 0), stopped).await {
        ServiceEnd::Stopped => 0,
        ServiceEnd::NotStarted => 1,
        ServiceEnd::Ended => BROKER_ENDED,
    }
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
    let (tx, rx) = watch::channel(false);
    let _ = STOP.set(tx);
    let layout = LAYOUT.get();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let code = match (layout, runtime) {
        (Some(layout), Ok(rt)) => {
            let code = rt.block_on(serve(layout, rx));
            rt.shutdown_timeout(Duration::from_secs(10));
            code
        }
        (Some(layout), Err(e)) => {
            service_run::service_log(layout, &format!("no async runtime: {e}"));
            1
        }
        (None, _) => 1,
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
