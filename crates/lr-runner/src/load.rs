use crate::agent::Shared;
use lr_abstractions::LoadSample;
use lr_proto::WireLoad;

pub(crate) fn wire_load(sample: LoadSample, free_disk_mb: Option<u64>) -> WireLoad {
    WireLoad {
        free_mem_mb: sample.free_mem_mb,
        cpu_busy_percent: sample.cpu_busy_percent.min(100),
        cpu_cores: sample.cpu_cores,
        free_disk_mb,
    }
}

impl Shared {
    /// The load of this machine for the next heartbeat, `None` when the runner has no reader or
    /// the reading is unavailable. The figure covers the whole machine, jobs of localrunner
    /// included: the broker takes its own reservations out, so nothing is counted twice.
    pub(crate) fn machine_load(&self) -> Option<WireLoad> {
        let sample = self.deps.load.as_ref()?.sample()?;
        let root = self.deps.settings.get().job_root.clone();
        Some(wire_load(sample, self.deps.disk.free_mb(&root)))
    }
}
