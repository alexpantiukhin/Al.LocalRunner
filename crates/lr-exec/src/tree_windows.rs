use crate::os_windows::handle_start;
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use tokio::process::{Child, Command};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GenerateConsoleCtrlEvent, GetConsoleProcessList, CTRL_BREAK_EVENT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    THREAD_SUSPEND_RESUME,
};

pub(crate) struct Placement;

pub(crate) struct Tree {
    job: HANDLE,
    pid: u32,
    start: Option<u64>,
}

unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}

fn has_console() -> bool {
    let mut pids = [0u32; 1];
    unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 1) != 0 }
}

impl Tree {
    pub(crate) fn shell(
        command: &str,
        _job_vars: &[&str],
    ) -> (Command, Vec<(String, String)>, Placement) {
        let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut cmd = Command::new(comspec);
        cmd.raw_arg(format!("/D /S /C \"{command}\""));
        (cmd, Vec::new(), Placement)
    }

    pub(crate) fn prepare(cmd: &mut Command) {
        let mut flags = CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED;
        if !has_console() {
            flags |= CREATE_NO_WINDOW;
        }
        cmd.creation_flags(flags);
    }

    pub(crate) fn attach(child: &Child, _placement: Placement) -> io::Result<Tree> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("process already exited"))?;
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("process handle unavailable"))?
            as HANDLE;
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let tree = Tree {
                job,
                pid,
                start: handle_start(process),
            };
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
                || AssignProcessToJobObject(job, process) == 0
            {
                return Err(io::Error::last_os_error());
            }
            resume_threads(pid)?;
            Ok(tree)
        }
    }

    pub(crate) fn start(&self) -> Option<u64> {
        self.start
    }

    pub(crate) fn observe(&self) {}

    pub(crate) fn soft_stop(&self) -> bool {
        self.pid != 0
            && has_console()
            && unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } != 0
    }

    pub(crate) fn kill(&self) {
        unsafe {
            TerminateJobObject(self.job, 1);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            let ok = QueryInformationJobObject(
                self.job,
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            );
            ok != 0 && info.ActiveProcesses == 0
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.job, 1);
            CloseHandle(self.job);
        }
    }
}

unsafe fn resume_threads(pid: u32) -> io::Result<()> {
    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry: THREADENTRY32 = zeroed();
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut resumed = false;
    let mut more = Thread32First(snapshot, &mut entry) != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
            if !thread.is_null() {
                ResumeThread(thread);
                CloseHandle(thread);
                resumed = true;
            }
        }
        more = Thread32Next(snapshot, &mut entry) != 0;
    }
    CloseHandle(snapshot);
    if resumed {
        Ok(())
    } else {
        Err(io::Error::other("could not resume suspended process"))
    }
}
