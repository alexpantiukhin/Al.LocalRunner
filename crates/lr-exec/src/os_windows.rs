use crate::probe::Probe;
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn open(pid: u32, access: u32) -> Result<Owned, u32> {
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
        Err(unsafe { GetLastError() })
    } else {
        Ok(Owned(handle))
    }
}

pub(crate) fn handle_start(handle: HANDLE) -> Option<u64> {
    unsafe {
        let mut created: FILETIME = zeroed();
        let mut exited: FILETIME = zeroed();
        let mut kernel: FILETIME = zeroed();
        let mut user: FILETIME = zeroed();
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return None;
        }
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

fn has_exited(handle: HANDLE, synchronize: bool) -> bool {
    unsafe {
        if synchronize {
            return WaitForSingleObject(handle, 0) == WAIT_OBJECT_0;
        }
        let mut code = 0u32;
        GetExitCodeProcess(handle, &mut code) != 0 && code != STILL_ACTIVE as u32
    }
}

pub(crate) fn probe(pid: u32) -> Probe {
    if pid == 0 {
        return Probe::Unreadable;
    }
    let (handle, synchronize) =
        match open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE) {
            Ok(h) => (h, true),
            Err(ERROR_INVALID_PARAMETER) => return Probe::Gone,
            Err(_) => match open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
                Ok(h) => (h, false),
                Err(ERROR_INVALID_PARAMETER) => return Probe::Gone,
                Err(_) => return Probe::Unreadable,
            },
        };
    if has_exited(handle.0, synchronize) {
        return Probe::Gone;
    }
    match handle_start(handle.0) {
        Some(start) => Probe::Alive(start),
        None => Probe::Unreadable,
    }
}

pub(crate) fn terminate(pid: u32, start: u64) {
    let Ok(handle) = open(pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION) else {
        return;
    };
    if handle_start(handle.0) == Some(start) {
        unsafe {
            TerminateProcess(handle.0, 1);
        }
    }
}

pub(crate) fn parents() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return map;
        }
        let snapshot = Owned(snapshot);
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot.0, &mut entry) != 0;
        while more {
            map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            more = Process32NextW(snapshot.0, &mut entry) != 0;
        }
    }
    map
}
