use crate::host::{DiskSpace, KeepAwake};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Free space set per volume root; a path belongs to the longest root it starts with. Without
/// a matching root nothing is measured. Clones share the same volumes.
#[derive(Clone, Default)]
pub struct FakeDiskSpace {
    volumes: Arc<Mutex<BTreeMap<PathBuf, u64>>>,
}

impl FakeDiskSpace {
    /// No volumes.
    pub fn new() -> FakeDiskSpace {
        FakeDiskSpace::default()
    }

    /// Sets the free megabytes of the volume mounted at `root`.
    pub fn set(&self, root: &Path, free_mb: u64) {
        self.volumes
            .lock()
            .unwrap()
            .insert(root.to_path_buf(), free_mb);
    }
}

impl DiskSpace for FakeDiskSpace {
    fn free_mb(&self, path: &Path) -> Option<u64> {
        self.volumes
            .lock()
            .unwrap()
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(_, free)| *free)
    }
}

#[derive(Default)]
struct Awake {
    held: bool,
    holds: usize,
    releases: usize,
}

/// Records holds and releases instead of touching the power settings of the host.
#[derive(Clone, Default)]
pub struct FakeKeepAwake {
    state: Arc<Mutex<Awake>>,
}

impl FakeKeepAwake {
    /// Not held.
    pub fn new() -> FakeKeepAwake {
        FakeKeepAwake::default()
    }

    /// Whether the machine is kept awake now.
    pub fn held(&self) -> bool {
        self.state.lock().unwrap().held
    }

    /// Holds that started keeping it awake and releases that ended it.
    pub fn counts(&self) -> (usize, usize) {
        let s = self.state.lock().unwrap();
        (s.holds, s.releases)
    }
}

impl KeepAwake for FakeKeepAwake {
    fn hold(&self) {
        let mut s = self.state.lock().unwrap();
        if !s.held {
            s.held = true;
            s.holds += 1;
        }
    }

    fn release(&self) {
        let mut s = self.state.lock().unwrap();
        if s.held {
            s.held = false;
            s.releases += 1;
        }
    }
}
