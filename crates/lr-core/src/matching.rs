use crate::model::Zone;
use crate::requirements::{MachineRef, Requirements};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// State of a runner service as far as matching is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Declared and usable.
    Available,
    /// Restart limit exhausted: the runner does not suit jobs that need it.
    Unavailable,
}

/// `[accept]` of `runner.toml`: a reserve for heavy orders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Accept {
    /// Only orders needing at least this much memory are accepted.
    pub min_mem_mb: Option<u64>,
}

/// What a runner reports about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Platform labels of the owner.
    pub platforms: BTreeSet<String>,
    /// Memory capacity in MB.
    pub mem_mb: u64,
    /// CPU capacity.
    pub cpu: u32,
    /// Free disk space in MB.
    pub disk_mb: u64,
    /// Programs found (and checked) on the runner.
    pub programs: BTreeSet<String>,
    /// Declared tags.
    pub tags: BTreeSet<String>,
    /// `max_job_timeout_s`; `None` means no ceiling.
    pub max_job_timeout_s: Option<u64>,
    /// Declared services.
    pub services: BTreeMap<String, ServiceStatus>,
    /// Zone of the runner relative to the client.
    pub zone: Zone,
    /// Machine reference of the runner, see [`MachineRef`].
    pub machine: Option<MachineRef>,
    /// Reserve rule of the owner.
    pub accept: Accept,
    /// Command names the owner declared for clients limited to named commands.
    pub commands: BTreeSet<String>,
}

/// One reason a runner does not suit an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MismatchReason {
    /// The runner has none of the platform.
    Platform {
        /// Needed platform.
        needed: String,
    },
    /// The order must run on the client's machine.
    NotSameMachine,
    /// Not enough memory.
    Memory {
        /// Needed MB.
        needed: u64,
        /// Runner MB.
        available: u64,
    },
    /// Not enough CPU.
    Cpu {
        /// Needed CPU.
        needed: u32,
        /// Runner CPU.
        available: u32,
    },
    /// Not enough disk.
    Disk {
        /// Needed MB.
        needed: u64,
        /// Free MB.
        available: u64,
    },
    /// A program is missing.
    Program {
        /// Program name.
        name: String,
    },
    /// A tag is not declared.
    Tag {
        /// Tag name.
        name: String,
    },
    /// The job timeout exceeds the runner ceiling.
    Timeout {
        /// Needed seconds.
        needed: u64,
        /// Ceiling seconds.
        ceiling: u64,
    },
    /// A service is not declared.
    ServiceMissing {
        /// Service name.
        name: String,
    },
    /// A service is unavailable.
    ServiceUnavailable {
        /// Service name.
        name: String,
    },
    /// The runner is farther than the order allows.
    Zone {
        /// Zone of the runner.
        runner: Zone,
        /// Farthest allowed zone.
        allowed: Zone,
    },
    /// A command name of the order is not declared by the runner.
    CommandMissing {
        /// Command name.
        name: String,
    },
    /// The order is lighter than the owner's reserve.
    BelowAccept {
        /// Memory of the order.
        needed: u64,
        /// Required minimum.
        min: u64,
    },
}

impl fmt::Display for MismatchReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Platform { needed } => write!(f, "платформа {needed} не заявлена"),
            Self::NotSameMachine => write!(f, "раннер не на машине клиента"),
            Self::Memory { needed, available } => {
                write!(f, "нужно {needed} МБ, у раннера {available}")
            }
            Self::Cpu { needed, available } => {
                write!(f, "нужно {needed} CPU, у раннера {available}")
            }
            Self::Disk { needed, available } => {
                write!(f, "нужно {needed} МБ диска, свободно {available}")
            }
            Self::Program { name } => write!(f, "{name} не найден"),
            Self::Tag { name } => write!(f, "тег {name} не заявлен"),
            Self::Timeout { needed, ceiling } => {
                write!(f, "нужен таймаут {needed} с, потолок раннера {ceiling} с")
            }
            Self::ServiceMissing { name } => write!(f, "сервис {name} не описан"),
            Self::ServiceUnavailable { name } => write!(f, "сервис {name} недоступен"),
            Self::Zone { runner, allowed } => {
                write!(f, "зона раннера {runner}, допустима не дальше {allowed}")
            }
            Self::CommandMissing { name } => write!(f, "команда {name} не описана"),
            Self::BelowAccept { needed, min } => {
                write!(f, "заказ на {needed} МБ легче допуска раннера {min} МБ")
            }
        }
    }
}

/// Every reason a runner does not suit an order; never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// The reasons, in a fixed order.
    pub reasons: Vec<MismatchReason>,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.reasons.iter().map(|r| r.to_string()).collect();
        f.write_str(&parts.join("; "))
    }
}

impl std::error::Error for Mismatch {}

/// Checks that a runner has everything the order requires (spec 4, 3.10, 3.11).
///
/// Memory and CPU are compared with the runner capacity; the free pool budget is
/// the scheduler's concern.
pub fn satisfies(caps: &Capabilities, req: &Requirements) -> Result<(), Mismatch> {
    let mut reasons = Vec::new();
    if let Some(platform) = &req.platform {
        if !caps.platforms.contains(platform) {
            reasons.push(MismatchReason::Platform {
                needed: platform.clone(),
            });
        }
    }
    if let Some(machine) = &req.same_machine {
        if caps.machine.as_ref() != Some(machine) {
            reasons.push(MismatchReason::NotSameMachine);
        }
    }
    if req.mem_mb > caps.mem_mb {
        reasons.push(MismatchReason::Memory {
            needed: req.mem_mb,
            available: caps.mem_mb,
        });
    }
    if req.cpu > caps.cpu {
        reasons.push(MismatchReason::Cpu {
            needed: req.cpu,
            available: caps.cpu,
        });
    }
    if req.disk_mb > caps.disk_mb {
        reasons.push(MismatchReason::Disk {
            needed: req.disk_mb,
            available: caps.disk_mb,
        });
    }
    for program in &req.programs {
        if !caps.programs.contains(&program.name) {
            reasons.push(MismatchReason::Program {
                name: program.name.clone(),
            });
        }
    }
    for tag in req.tags.union(&req.exclusive) {
        if !caps.tags.contains(tag) {
            reasons.push(MismatchReason::Tag { name: tag.clone() });
        }
    }
    if let Some(ceiling) = caps.max_job_timeout_s {
        if req.timeout_s > ceiling {
            reasons.push(MismatchReason::Timeout {
                needed: req.timeout_s,
                ceiling,
            });
        }
    }
    for service in &req.services {
        match caps.services.get(service) {
            None => reasons.push(MismatchReason::ServiceMissing {
                name: service.clone(),
            }),
            Some(ServiceStatus::Unavailable) => reasons.push(MismatchReason::ServiceUnavailable {
                name: service.clone(),
            }),
            Some(ServiceStatus::Available) => {}
        }
    }
    for name in &req.commands {
        if !caps.commands.contains(name) {
            reasons.push(MismatchReason::CommandMissing { name: name.clone() });
        }
    }
    if let Some(allowed) = req.max_zone {
        if caps.zone > allowed {
            reasons.push(MismatchReason::Zone {
                runner: caps.zone,
                allowed,
            });
        }
    }
    if let Some(min) = caps.accept.min_mem_mb {
        if req.mem_mb < min {
            reasons.push(MismatchReason::BelowAccept {
                needed: req.mem_mb,
                min,
            });
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(Mismatch { reasons })
    }
}
