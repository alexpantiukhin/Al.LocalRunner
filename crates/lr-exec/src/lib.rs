//! Process execution of the runner (spec 6, 7.3, 3.11, 3.13): [`ExecRunner`] implements
//! [`lr_abstractions::ProcessRunner`] with real processes and whole-tree termination,
//! [`OsProcessInspector`] implements [`lr_abstractions::ProcessInspector`] by pid and start
//! fingerprint, and [`SystemClock`] is the real [`lr_abstractions::Clock`].
//!
//! Job output stays bytes end to end: [`Masker`] hides secret values even when they are split
//! between reads, and [`LogWriter`] writes the job log with bounded lines; each step's output
//! begins with a [`header_line`].

mod clock;
#[cfg(unix)]
mod guard;
mod job;
mod log;
mod mask;
#[cfg(unix)]
mod os_unix;
#[cfg(windows)]
mod os_windows;
mod probe;
mod runner;
mod spawn;
mod step;
#[cfg(unix)]
mod tree_unix;
#[cfg(windows)]
mod tree_windows;

pub use clock::SystemClock;
pub use log::{cancel_line, header_line, LogWriter, MAX_LINE, SOFT_STOP_NOT_DELIVERED};
pub use mask::Masker;
pub use probe::{process_start, OsProcessInspector};
pub use runner::ExecRunner;
pub use spawn::MAX_CHUNK;
