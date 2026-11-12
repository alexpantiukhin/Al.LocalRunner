use lr_core::{Admission, ForeignLoadMode, DEFAULT_MEM_HEADROOM_MB};
use serde::Deserialize;

/// `[admission] foreign_load`: whether the real load of a runner machine gates a start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForeignLoad {
    /// Only the declared pool budget decides (default).
    #[default]
    Off,
    /// An order waits while the machine of the runner has too little free memory or CPU.
    Wait,
}

/// `[admission]`: admission by the real load of the runner machine. Without the table nothing
/// changes: the declared pool budget alone decides. Runners that send no load are never held
/// back by it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdmissionSection {
    /// `foreign_load`, default `off`.
    pub foreign_load: ForeignLoad,
    /// `mem_headroom_mb`: memory that must stay free on top of the order's `mem_mb`, default 256.
    pub mem_headroom_mb: u64,
}

impl Default for AdmissionSection {
    fn default() -> Self {
        AdmissionSection {
            foreign_load: ForeignLoad::Off,
            mem_headroom_mb: DEFAULT_MEM_HEADROOM_MB,
        }
    }
}

impl AdmissionSection {
    /// The core view of the section.
    pub fn admission(&self) -> Admission {
        Admission {
            foreign_load: match self.foreign_load {
                ForeignLoad::Off => ForeignLoadMode::Off,
                ForeignLoad::Wait => ForeignLoadMode::Wait,
            },
            mem_headroom_mb: self.mem_headroom_mb,
        }
    }
}
