use crate::limits::{backstop_mb, bytes_of, cpu_rate, mb_of};
use lr_abstractions::{Enforce, ResourceLimits};
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub(crate) fn apply(job: HANDLE, limits: &ResourceLimits) -> io::Result<()> {
    let enforcing = limits.mode != Enforce::Off;
    let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    if let (true, Some(procs)) = (enforcing, limits.procs) {
        flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = procs.max(1);
    }
    if let (Enforce::Kill, Some(mem)) = (limits.mode, limits.mem_mb) {
        flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = usize::try_from(bytes_of(backstop_mb(mem))).unwrap_or(usize::MAX);
    }
    info.BasicLimitInformation.LimitFlags = flags;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if let (true, Some(millis)) = (enforcing, limits.cpu_millis) {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        let mut rate: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        rate.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        rate.Anonymous.CpuRate = cpu_rate(millis, cpus);
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectCpuRateControlInformation,
                &rate as *const _ as *const _,
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            );
        }
    }
    Ok(())
}

pub(crate) fn peak_job_mb(job: HANDLE) -> Option<u64> {
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            null_mut(),
        )
    };
    (ok != 0).then(|| mb_of(info.PeakJobMemoryUsed as u64))
}
