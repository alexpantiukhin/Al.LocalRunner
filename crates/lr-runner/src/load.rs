use crate::agent::Shared;
use lr_abstractions::LoadSample;
use lr_proto::WireLoad;

pub(crate) fn wire_load(sample: LoadSample) -> WireLoad {
    WireLoad {
        free_mem_mb: sample.free_mem_mb,
        cpu_busy_percent: sample.cpu_busy_percent.min(100),
        cpu_cores: sample.cpu_cores,
    }
}

impl Shared {
    /// The load of this machine for the next heartbeat, `None` when the runner has no reader or
    /// the reading is unavailable. The figure covers the whole machine, jobs of localrunner
    /// included: the broker takes its own reservations out, so nothing is counted twice.
    pub(crate) fn machine_load(&self) -> Option<WireLoad> {
        self.deps.load.as_ref()?.sample().map(wire_load)
    }
}
