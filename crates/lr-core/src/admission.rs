//! Admission by the real load of the runner's machine (spec 6, issue 35 item 4): besides the
//! declared pool budget the broker looks at what the machine really has free.

use crate::requirements::Requirements;
use crate::scheduler::Wait;
use crate::zone::Endpoint;
use std::net::IpAddr;

/// `[admission] foreign_load`: what the broker does with the real load of a runner machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForeignLoadMode {
    /// The real load is ignored; only the declared pool budget decides (default).
    #[default]
    Off,
    /// An order waits while the machine of the runner has too little memory or CPU free.
    Wait,
}

/// `[admission]` of the broker policy. The default keeps the behaviour without the section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    /// `foreign_load`, default `off`.
    pub foreign_load: ForeignLoadMode,
    /// `mem_headroom_mb`: memory that must stay free on top of the order's `mem_mb`.
    pub mem_headroom_mb: u64,
}

/// Default `[admission] mem_headroom_mb`.
pub const DEFAULT_MEM_HEADROOM_MB: u64 = 256;

impl Default for Admission {
    fn default() -> Self {
        Admission {
            foreign_load: ForeignLoadMode::Off,
            mem_headroom_mb: DEFAULT_MEM_HEADROOM_MB,
        }
    }
}

/// The last load a runner reported about its whole machine, jobs of localrunner included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineLoad {
    /// Memory the OS can give to a new process, MB.
    pub free_mem_mb: u64,
    /// Share of CPU time busy over the last interval, 0 to 100.
    pub cpu_busy_percent: u8,
    /// Logical cores of the machine.
    pub cpu_cores: u32,
}

/// Which resource the foreign load takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadResource {
    /// Memory.
    Memory,
    /// CPU.
    Cpu,
}

fn is_local(e: &Endpoint, broker_addrs: &[IpAddr]) -> bool {
    match e {
        Endpoint::Ipc { .. } => true,
        Endpoint::Addr(a) => a.is_loopback() || broker_addrs.contains(a),
    }
}

/// Whether two runners run on one machine as the broker can tell: both local to the broker
/// host, or connected from one address.
pub fn same_host(a: &Endpoint, b: &Endpoint, broker_addrs: &[IpAddr]) -> bool {
    (is_local(a, broker_addrs) && is_local(b, broker_addrs)) || a == b
}

impl Admission {
    /// Whether an order of `req` may start on a machine with `load`.
    ///
    /// `own_cpu` is the declared CPU of the jobs of this broker on that machine: it is taken out
    /// of the busy share, so what those jobs use is never counted as foreign. `fresh_mem_mb` is
    /// the memory declared by jobs assigned after the sample was taken, which have not claimed
    /// it yet.
    pub fn check(
        &self,
        load: &MachineLoad,
        req: &Requirements,
        own_cpu: u32,
        fresh_mem_mb: u64,
    ) -> Result<(), Wait> {
        if self.foreign_load == ForeignLoadMode::Off {
            return Ok(());
        }
        let available = load.free_mem_mb.saturating_sub(fresh_mem_mb);
        let needed = req.mem_mb.saturating_add(self.mem_headroom_mb);
        if available < needed {
            return Err(Wait::ForeignLoad {
                resource: LoadResource::Memory,
                needed,
                level: available,
            });
        }
        let cores = u64::from(load.cpu_cores);
        if cores == 0 {
            return Ok(());
        }
        let busy = u64::from(load.cpu_busy_percent.min(100)) * cores;
        let foreign = busy.saturating_sub(u64::from(own_cpu) * 100);
        let free = (cores * 100).saturating_sub(foreign);
        if free < u64::from(req.cpu) * 100 {
            return Err(Wait::ForeignLoad {
                resource: LoadResource::Cpu,
                needed: u64::from(req.cpu),
                level: foreign.div_ceil(cores).min(100),
            });
        }
        Ok(())
    }
}
