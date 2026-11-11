//! Facts and effects of the host machine a runner needs besides processes and files: the free
//! space of a volume (`disk_mb`, spec 4) and keeping the machine awake while a job runs
//! (spec 10, "Запрет сна").

use std::path::Path;

/// Free space of the volume that holds a path.
pub trait DiskSpace: Send + Sync {
    /// Free megabytes on the volume of `path` (its nearest existing ancestor); `None` when it
    /// cannot be measured.
    fn free_mb(&self, path: &Path) -> Option<u64>;
}

/// Keeps the machine from going to sleep while held. Holding twice is one hold; releasing
/// when not held does nothing. Ending the process ends the hold.
pub trait KeepAwake: Send + Sync {
    /// Starts keeping the machine awake; the error says why the OS did not take the hold, and
    /// the machine is then not held.
    fn hold(&self) -> Result<(), String>;

    /// Lets the machine sleep again.
    fn release(&self);
}
