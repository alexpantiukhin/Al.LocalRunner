use lr_proto::{WireEnforce, WireLimits};
use serde::Deserialize;

/// Default `mem_headroom` and `disk_headroom` factor.
pub const DEFAULT_HEADROOM: f64 = 1.25;

const MAX_HEADROOM: f64 = 100.0;

/// What happens to a job above its ceiling (`[resources] mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceMode {
    /// Only the peaks are recorded.
    Off,
    /// A warning goes to the job log (default).
    #[default]
    Warn,
    /// The job is killed with the reason `resource_exceeded`.
    Kill,
}

/// A margin factor of at least 1.0, kept as whole percent (1.25 is 125).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "f64")]
pub struct Headroom(u32);

impl Headroom {
    /// The factor in whole percent.
    pub fn percent(self) -> u32 {
        self.0
    }

    /// The factor.
    pub fn factor(self) -> f64 {
        f64::from(self.0) / 100.0
    }
}

impl Default for Headroom {
    fn default() -> Self {
        Headroom(125)
    }
}

impl TryFrom<f64> for Headroom {
    type Error = String;

    fn try_from(v: f64) -> Result<Self, String> {
        if v.is_finite() && (1.0..=MAX_HEADROOM).contains(&v) {
            Ok(Headroom((v * 100.0).round() as u32))
        } else {
            Err(format!(
                "{v} is not a factor from 1.0 to {MAX_HEADROOM}; the ceiling of a job is its declared value times the factor"
            ))
        }
    }
}

/// `[resources]`: enforcement of the declared `mem_mb` and `disk_mb`. Without the table the
/// broker sends no limits and the runner uses its own default (`warn`). Declared CPU and the process count
/// are limited by the runner whenever its OS supports it and are not set here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResourcesSection {
    /// `mode`, default `warn`.
    pub mode: ResourceMode,
    /// `mem_headroom`: memory ceiling as a factor of the declared `mem_mb`, default 1.25.
    pub mem_headroom: Headroom,
    /// `disk_headroom`: disk ceiling as a factor of the declared `disk_mb`, default 1.25.
    pub disk_headroom: Headroom,
}

impl Default for ResourcesSection {
    fn default() -> Self {
        ResourcesSection {
            mode: ResourceMode::Warn,
            mem_headroom: Headroom::default(),
            disk_headroom: Headroom::default(),
        }
    }
}

impl ResourcesSection {
    /// The part of the section that travels with an assignment.
    pub fn wire(&self) -> WireLimits {
        WireLimits {
            mode: match self.mode {
                ResourceMode::Off => WireEnforce::Off,
                ResourceMode::Warn => WireEnforce::Warn,
                ResourceMode::Kill => WireEnforce::Kill,
            },
            mem_headroom_pct: self.mem_headroom.percent(),
            disk_headroom_pct: self.disk_headroom.percent(),
        }
    }
}
