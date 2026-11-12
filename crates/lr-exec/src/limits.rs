use lr_abstractions::{Enforce, Exceeded, LimitedResource};

const BYTES_PER_MB: u64 = 1024 * 1024;

pub(crate) fn mb_of(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_MB)
}

pub(crate) fn bytes_of(mb: u64) -> u64 {
    mb.saturating_mul(BYTES_PER_MB)
}

/// The OS-level hard limit set a little above the ceiling the watcher enforces, so that the
/// watcher ends the job with a proper reason before the OS refuses allocations or kills it.
pub(crate) fn backstop_mb(ceiling_mb: u64) -> u64 {
    ceiling_mb
        .saturating_add(ceiling_mb / 10)
        .max(ceiling_mb + 1)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn cpu_rate(cpu_millis: u32, logical_cpus: u32) -> u32 {
    let cpus = u64::from(logical_cpus.max(1));
    let rate = u64::from(cpu_millis) * 10_000 / (1000 * cpus);
    rate.clamp(1, 10_000) as u32
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Within,
    Warn(Exceeded),
    Kill(Exceeded),
}

pub(crate) struct Meter {
    resource: LimitedResource,
    ceiling: Option<u64>,
    declared: Option<u64>,
    mode: Enforce,
    peak: Option<u64>,
    warned: bool,
}

impl Meter {
    pub(crate) fn new(
        resource: LimitedResource,
        ceiling: Option<u64>,
        declared: Option<u64>,
        mode: Enforce,
    ) -> Meter {
        Meter {
            resource,
            ceiling,
            declared,
            mode,
            peak: None,
            warned: false,
        }
    }

    pub(crate) fn peak(&self) -> Option<u64> {
        self.peak
    }

    pub(crate) fn observe(&mut self, mb: u64) -> Verdict {
        let peak = self.peak.map_or(mb, |p| p.max(mb));
        self.peak = Some(peak);
        let Some(limit) = self.ceiling else {
            return Verdict::Within;
        };
        if mb <= limit {
            return Verdict::Within;
        }
        let exceeded = Exceeded {
            resource: self.resource,
            limit_mb: limit,
            declared_mb: self.declared,
            peak_mb: peak,
        };
        match self.mode {
            Enforce::Off => Verdict::Within,
            Enforce::Kill => Verdict::Kill(exceeded),
            Enforce::Warn if self.warned => Verdict::Within,
            Enforce::Warn => {
                self.warned = true;
                Verdict::Warn(exceeded)
            }
        }
    }
}

fn names(resource: LimitedResource) -> (&'static str, &'static str) {
    match resource {
        LimitedResource::Memory => ("memory", "mem_mb"),
        LimitedResource::Disk => ("disk", "disk_mb"),
    }
}

pub(crate) fn warn_line(ex: &Exceeded) -> String {
    let (what, key) = names(ex.resource);
    match ex.declared_mb {
        Some(declared) => format!(
            "localrunner: {what} peak {} MB exceeds the declared {key} {declared} MB (ceiling {} MB)\n",
            ex.peak_mb, ex.limit_mb
        ),
        None => format!(
            "localrunner: {what} peak {} MB exceeds the ceiling {} MB\n",
            ex.peak_mb, ex.limit_mb
        ),
    }
}

pub(crate) fn kill_line(ex: &Exceeded) -> String {
    let (_, key) = names(ex.resource);
    let declared = ex.declared_mb.unwrap_or(ex.limit_mb);
    format!(
        "localrunner: killed: declared {key} exceeded: declared {declared} MB, peak {} MB\n",
        ex.peak_mb
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter(mode: Enforce) -> Meter {
        Meter::new(LimitedResource::Memory, Some(1250), Some(1000), mode)
    }

    #[test]
    fn the_cpu_rate_is_a_share_of_all_logical_cores() {
        assert_eq!(cpu_rate(1000, 8), 1250);
        assert_eq!(cpu_rate(4000, 16), 2500);
        assert_eq!(cpu_rate(16_000, 8), 10_000);
        assert_eq!(cpu_rate(1, 64), 1);
        assert_eq!(cpu_rate(1000, 0), 10_000);
    }

    #[test]
    fn the_backstop_is_above_the_ceiling() {
        assert_eq!(backstop_mb(1000), 1100);
        assert_eq!(backstop_mb(5), 6);
        assert_eq!(mb_of(1), 1);
        assert_eq!(mb_of(BYTES_PER_MB + 1), 2);
    }

    #[test]
    fn the_peak_is_tracked_in_every_mode() {
        let mut m = meter(Enforce::Off);
        assert_eq!(m.observe(900), Verdict::Within);
        assert_eq!(m.observe(2000), Verdict::Within);
        assert_eq!(m.observe(10), Verdict::Within);
        assert_eq!(m.peak(), Some(2000));
    }

    #[test]
    fn warn_speaks_once_and_kill_every_time() {
        let mut warn = meter(Enforce::Warn);
        assert_eq!(warn.observe(1250), Verdict::Within);
        let Verdict::Warn(ex) = warn.observe(1500) else {
            panic!("expected a warning");
        };
        assert_eq!((ex.limit_mb, ex.peak_mb), (1250, 1500));
        assert_eq!(warn.observe(1600), Verdict::Within);
        let mut kill = meter(Enforce::Kill);
        assert!(matches!(kill.observe(1251), Verdict::Kill(_)));
        assert!(matches!(kill.observe(1251), Verdict::Kill(_)));
    }

    #[test]
    fn no_ceiling_never_exceeds() {
        let mut m = Meter::new(LimitedResource::Disk, None, None, Enforce::Kill);
        assert_eq!(m.observe(u64::MAX), Verdict::Within);
    }

    #[test]
    fn the_lines_name_the_declared_value_and_the_peak() {
        let ex = Exceeded {
            resource: LimitedResource::Memory,
            limit_mb: 1250,
            declared_mb: Some(1000),
            peak_mb: 1500,
        };
        assert_eq!(
            warn_line(&ex),
            "localrunner: memory peak 1500 MB exceeds the declared mem_mb 1000 MB (ceiling 1250 MB)\n"
        );
        assert_eq!(
            kill_line(&ex),
            "localrunner: killed: declared mem_mb exceeded: declared 1000 MB, peak 1500 MB\n"
        );
        let disk = Exceeded {
            resource: LimitedResource::Disk,
            declared_mb: None,
            ..ex
        };
        assert!(warn_line(&disk).starts_with("localrunner: disk peak 1500 MB exceeds the ceiling"));
    }
}
