use crate::load::{HostLoad, LoadSample};
use std::sync::{Arc, Mutex};

/// A host load that tests set; without a value nothing is measured. Clones share the value.
#[derive(Clone, Default)]
pub struct FakeHostLoad {
    sample: Arc<Mutex<Option<LoadSample>>>,
}

impl FakeHostLoad {
    /// Nothing measured.
    pub fn new() -> FakeHostLoad {
        FakeHostLoad::default()
    }

    /// Sets what the next readings return; `None` means the load cannot be measured.
    pub fn set(&self, sample: Option<LoadSample>) {
        *self.sample.lock().unwrap() = sample;
    }
}

impl HostLoad for FakeHostLoad {
    fn sample(&self) -> Option<LoadSample> {
        *self.sample.lock().unwrap()
    }
}
