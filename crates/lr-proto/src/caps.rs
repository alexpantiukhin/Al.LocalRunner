use lr_core::{Accept, Capabilities, MachineLoad, MachineRef, ServiceStatus, Zone};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// State of a declared service on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireServiceStatus {
    /// Declared and usable.
    Available,
    /// Restart limit exhausted.
    Unavailable,
    /// A status of a newer minor version, treated as unavailable.
    #[serde(other)]
    Unknown,
}

/// Real load of the machine of a runner, sent with each heartbeat (issue 35, item 4). It covers
/// the whole machine, jobs of localrunner included; the broker takes its own reservations out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLoad {
    /// Memory the OS can give to a new process, MB.
    pub free_mem_mb: u64,
    /// Share of CPU time busy since the previous sample, 0 to 100.
    pub cpu_busy_percent: u8,
    /// Logical cores of the machine.
    pub cpu_cores: u32,
}

impl From<WireLoad> for MachineLoad {
    fn from(l: WireLoad) -> Self {
        MachineLoad {
            free_mem_mb: l.free_mem_mb,
            cpu_busy_percent: l.cpu_busy_percent.min(100),
            cpu_cores: l.cpu_cores,
        }
    }
}

/// What a runner offers, as it travels. The zone is not part of it: the broker computes it per
/// client and never trusts the runner's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCapabilities {
    /// Platform labels.
    pub platforms: BTreeSet<String>,
    /// Memory capacity in MB.
    pub mem_mb: u64,
    /// CPU capacity.
    pub cpu: u32,
    /// Free disk in MB.
    pub disk_mb: u64,
    /// Programs found and checked.
    pub programs: BTreeSet<String>,
    /// Declared tags.
    pub tags: BTreeSet<String>,
    /// `max_job_timeout_s`, `None` for no ceiling.
    pub max_job_timeout_s: Option<u64>,
    /// Declared services.
    pub services: BTreeMap<String, WireServiceStatus>,
    /// Machine reference of the runner.
    pub machine: Option<MachineRef>,
    /// `[accept] min_mem_mb`.
    pub accept_min_mem_mb: Option<u64>,
    /// Cache size limit in MB, for the runners page.
    pub cache_max_mb: Option<u64>,
    /// Cache fill in MB.
    pub cache_used_mb: Option<u64>,
    /// `memory_mb` of each declared service, so the broker reserves it before the service
    /// starts (spec 3.11); absent from runners of an earlier minor version.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub service_mem_mb: BTreeMap<String, u64>,
    /// Command names of `runner.toml` `[commands]`, for clients limited to named commands
    /// (spec 3.5); absent from runners of an earlier minor version.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub commands: BTreeSet<String>,
}

impl From<&Capabilities> for WireCapabilities {
    fn from(c: &Capabilities) -> Self {
        WireCapabilities {
            platforms: c.platforms.clone(),
            mem_mb: c.mem_mb,
            cpu: c.cpu,
            disk_mb: c.disk_mb,
            programs: c.programs.clone(),
            tags: c.tags.clone(),
            max_job_timeout_s: c.max_job_timeout_s,
            services: c
                .services
                .iter()
                .map(|(n, s)| {
                    let s = match s {
                        ServiceStatus::Available => WireServiceStatus::Available,
                        ServiceStatus::Unavailable => WireServiceStatus::Unavailable,
                    };
                    (n.clone(), s)
                })
                .collect(),
            machine: c.machine.clone(),
            accept_min_mem_mb: c.accept.min_mem_mb,
            cache_max_mb: None,
            cache_used_mb: None,
            service_mem_mb: BTreeMap::new(),
            commands: c.commands.clone(),
        }
    }
}

impl WireCapabilities {
    /// The scheduling view; `zone` is the one the broker computed for this client.
    pub fn into_capabilities(self, zone: Zone) -> Capabilities {
        Capabilities {
            platforms: self.platforms,
            mem_mb: self.mem_mb,
            cpu: self.cpu,
            disk_mb: self.disk_mb,
            programs: self.programs,
            tags: self.tags,
            max_job_timeout_s: self.max_job_timeout_s,
            services: self
                .services
                .into_iter()
                .map(|(n, s)| {
                    let s = match s {
                        WireServiceStatus::Available => ServiceStatus::Available,
                        WireServiceStatus::Unavailable | WireServiceStatus::Unknown => {
                            ServiceStatus::Unavailable
                        }
                    };
                    (n, s)
                })
                .collect(),
            zone,
            machine: self.machine,
            accept: Accept {
                min_mem_mb: self.accept_min_mem_mb,
            },
            commands: self.commands,
        }
    }
}
