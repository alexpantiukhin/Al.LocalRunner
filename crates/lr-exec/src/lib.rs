//! Process execution of the runner (spec 6, 7.3, 3.11, 3.13): [`ExecRunner`] implements
//! [`lr_abstractions::ProcessRunner`] with real processes and whole-tree termination,
//! [`OsProcessInspector`] implements [`lr_abstractions::ProcessInspector`] by pid and start
//! fingerprint, [`SystemClock`] is the real [`lr_abstractions::Clock`] and [`SystemKeepAwake`]
//! the real [`lr_abstractions::KeepAwake`].
//!
//! Job output stays bytes end to end: [`Masker`] hides secret values even when they are split
//! between reads, and [`LogWriter`] writes the job log with bounded lines; each step's output
//! begins with a [`header_line`].

mod awake;
mod boot;
mod clock;
mod console;
#[cfg(unix)]
mod guard;
mod job;
mod limits;
#[cfg(windows)]
mod limits_windows;
mod log;
mod mask;
#[cfg(unix)]
mod mem_unix;
#[cfg(unix)]
mod os_unix;
#[cfg(windows)]
mod os_windows;
mod probe;
mod runner;
#[cfg(target_os = "linux")]
mod scope_linux;
mod spawn;
mod step;
#[cfg(unix)]
mod tree_unix;
#[cfg(windows)]
mod tree_windows;
mod watch;

#[cfg(windows)]
pub use awake::HOLD_FLAGS;
pub use awake::{caffeinate_command, system_keep_awake, Holder, Inhibitor, SystemKeepAwake};
pub use clock::SystemClock;
pub use console::ensure_console;
pub use log::{cancel_line, header_line, LogWriter, MAX_LINE, SOFT_STOP_NOT_DELIVERED};
pub use mask::Masker;
pub use probe::{process_start, OsProcessInspector};
pub use runner::ExecRunner;
pub use spawn::{os_env, MAX_CHUNK};

/// How this build contains a job's process tree (spec 3.11, 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    /// Windows Job Object: the whole tree, detached descendants included.
    JobObject,
    /// Linux cgroup v2 scope through `systemd-run --user --scope`: the whole tree.
    CgroupScope,
    /// Process group and the walk over descendants by parent: best effort.
    ProcessGroup,
}

/// The containment used for job trees here; on Linux the first call checks once whether
/// `systemd-run --user --scope` works (cgroup v2 and a user manager), else it falls back to the
/// process group.
pub fn containment() -> Containment {
    #[cfg(windows)]
    {
        Containment::JobObject
    }
    #[cfg(target_os = "linux")]
    {
        if scope_linux::available() {
            Containment::CgroupScope
        } else {
            Containment::ProcessGroup
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Containment::ProcessGroup
    }
}

/// Which mechanism limits the memory of a job here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// The OS limits the tree (Windows Job Object, delegated cgroup v2 controllers), with the
    /// user-space watcher on top for the exceeded reason.
    Os,
    /// Only the user-space watcher measures the tree and kills it (macOS, Linux without
    /// delegated controllers); approximate.
    Watcher,
}

/// The mechanism this build and host use for the memory of a job; the first call on Linux
/// probes `systemd-run --user --scope` once.
pub fn enforcement() -> Enforcement {
    #[cfg(windows)]
    {
        Enforcement::Os
    }
    #[cfg(target_os = "linux")]
    {
        if scope_linux::memory_delegated() {
            Enforcement::Os
        } else {
            Enforcement::Watcher
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Enforcement::Watcher
    }
}
