use lr_abstractions::{Enforce, Exceeded, LimitedResource, ResourceLimits};

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

/// The hard memory limit of a Windows Job Object: at the ceiling in limit mode (allocations
/// beyond it fail), at the backstop in kill mode, none otherwise.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn os_memory_limit_mb(limits: &ResourceLimits) -> Option<u64> {
    let mem = limits.mem_mb?;
    match limits.mode {
        Enforce::Limit => Some(mem),
        Enforce::Kill => Some(backstop_mb(mem)),
        Enforce::Off | Enforce::Warn => None,
    }
}

/// The `systemd-run -p` properties of a job scope for `limits` given the controllers the user
/// manager delegates. In kill mode the hard memory limit sits above the ceiling the watcher
/// enforces, so the watcher ends the job with the proper reason first; in limit mode the soft
/// limit `MemoryHigh` sits at the ceiling (the kernel throttles and reclaims, it does not
/// kill) and the hard limit stays at the backstop.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn scope_properties(limits: &ResourceLimits, controllers: &[String]) -> Vec<String> {
    let has = |name: &str| controllers.iter().any(|c| c == name);
    let mut out = Vec::new();
    if limits.mode == Enforce::Off {
        return out;
    }
    if let (Some(mem), true) = (limits.mem_mb, has("memory")) {
        let backstop = bytes_of(backstop_mb(mem));
        match limits.mode {
            Enforce::Kill => {
                out.push(format!("MemoryMax={backstop}"));
                out.push("MemorySwapMax=0".to_string());
                out.push("OOMPolicy=continue".to_string());
            }
            Enforce::Limit => {
                out.push(format!("MemoryHigh={}", bytes_of(mem)));
                out.push(format!("MemoryMax={backstop}"));
                out.push("OOMPolicy=continue".to_string());
            }
            Enforce::Off | Enforce::Warn => {}
        }
    }
    if let (Some(millis), true) = (limits.cpu_millis, has("cpu")) {
        out.push(format!("CPUQuota={}%", (millis / 10).max(1)));
    }
    if let (Some(procs), true) = (limits.procs, has("pids")) {
        out.push(format!("TasksMax={}", procs.max(1)));
    }
    out
}

/// The excess to report when a job held at its memory ceiling (limit mode) failed: a refused
/// allocation is an obscure error, so a non-zero exit with the peak within 10% of the ceiling
/// (or above it, where the OS limit is soft) is reported as the ceiling reached.
pub(crate) fn limit_exceeded(
    limits: &ResourceLimits,
    code: i32,
    peak_mb: Option<u64>,
) -> Option<Exceeded> {
    let ceiling = limits.mem_mb?;
    let peak = peak_mb?;
    if limits.mode != Enforce::Limit || code == 0 {
        return None;
    }
    if peak.saturating_mul(10) < ceiling.saturating_mul(9) {
        return None;
    }
    Some(Exceeded {
        resource: LimitedResource::Memory,
        limit_mb: ceiling,
        declared_mb: limits.declared_mem_mb,
        peak_mb: peak,
    })
}

/// What the OS remembers about a job's memory: kernel OOM kills of its scope, how often it hit
/// its hard limit, and its peak (the last needs a kernel with `memory.peak`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MemoryEvidence {
    pub(crate) oom_kills: u64,
    pub(crate) max_events: u64,
    pub(crate) peak_mb: u64,
}

const SIGKILL_CODE: i32 = 137;
const SIGTERM_CODE: i32 = 143;

/// The excess to report when the OS backstop, not the watcher, ended a job. The root exited
/// with 137 (SIGKILL: the kernel's OOM kill) or 143 (SIGTERM: the user manager stopping the
/// scope after that kill), the mode is kill with a memory ceiling, and the scope counted an OOM
/// kill or hit its hard limit; a 137 also counts when the scope peaked within 10% of the
/// backstop. `peak_mb` is the largest peak seen by the watcher.
pub(crate) fn oom_exceeded(
    limits: &ResourceLimits,
    code: i32,
    evidence: MemoryEvidence,
    peak_mb: Option<u64>,
) -> Option<Exceeded> {
    let ceiling = limits.mem_mb?;
    if limits.mode != Enforce::Kill || (code != SIGKILL_CODE && code != SIGTERM_CODE) {
        return None;
    }
    let backstop = backstop_mb(ceiling);
    let near =
        code == SIGKILL_CODE && evidence.peak_mb.saturating_mul(10) >= backstop.saturating_mul(9);
    if evidence.oom_kills == 0 && evidence.max_events == 0 && !near {
        return None;
    }
    Some(Exceeded {
        resource: LimitedResource::Memory,
        limit_mb: ceiling,
        declared_mb: limits.declared_mem_mb,
        peak_mb: backstop.max(evidence.peak_mb).max(peak_mb.unwrap_or(0)),
    })
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

    pub(crate) fn raise_peak(&mut self, mb: u64) {
        self.peak = Some(self.peak.map_or(mb, |p| p.max(mb)));
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
            Enforce::Warn | Enforce::Limit if self.warned => Verdict::Within,
            Enforce::Warn | Enforce::Limit => {
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

pub(crate) fn held_line(ex: &Exceeded) -> String {
    let (what, key) = names(ex.resource);
    let declared = ex.declared_mb.unwrap_or(ex.limit_mb);
    format!(
        "localrunner: failed at the {what} ceiling: declared {key} {declared} MB, ceiling {} MB, peak {} MB\n",
        ex.limit_mb, ex.peak_mb
    )
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

    fn kill_limits() -> ResourceLimits {
        ResourceLimits {
            mem_mb: Some(320),
            declared_mem_mb: Some(256),
            mode: Enforce::Kill,
            ..ResourceLimits::default()
        }
    }

    #[test]
    fn an_oom_kill_of_the_scope_is_an_excess() {
        let seen = MemoryEvidence {
            oom_kills: 1,
            max_events: 5,
            peak_mb: 340,
        };
        let ex = oom_exceeded(&kill_limits(), 137, seen, Some(200)).unwrap();
        assert_eq!(ex.resource, LimitedResource::Memory);
        assert_eq!(
            (ex.limit_mb, ex.declared_mb, ex.peak_mb),
            (320, Some(256), 352)
        );
        let peak = MemoryEvidence {
            oom_kills: 0,
            max_events: 0,
            peak_mb: 320,
        };
        assert!(oom_exceeded(&kill_limits(), 137, peak, None).is_some());
        let limit_hit = MemoryEvidence {
            max_events: 1,
            ..MemoryEvidence::default()
        };
        assert!(oom_exceeded(&kill_limits(), 137, limit_hit, None).is_some());
    }

    #[test]
    fn other_endings_are_not_an_excess() {
        let oom = MemoryEvidence {
            oom_kills: 1,
            max_events: 1,
            peak_mb: 352,
        };
        assert!(oom_exceeded(&kill_limits(), 1, oom, None).is_none());
        assert!(oom_exceeded(&kill_limits(), 143, oom, None).is_some());
        assert!(oom_exceeded(&kill_limits(), 130, oom, None).is_none());
        let near = MemoryEvidence {
            peak_mb: 340,
            ..MemoryEvidence::default()
        };
        assert!(oom_exceeded(&kill_limits(), 143, near, None).is_none());
        let low = MemoryEvidence {
            oom_kills: 0,
            max_events: 0,
            peak_mb: 100,
        };
        assert!(oom_exceeded(&kill_limits(), 137, low, Some(100)).is_none());
        let mut warn = kill_limits();
        warn.mode = Enforce::Warn;
        assert!(oom_exceeded(&warn, 137, oom, None).is_none());
        let mut none = kill_limits();
        none.mem_mb = None;
        assert!(oom_exceeded(&none, 137, oom, None).is_none());
    }

    fn limit_limits() -> ResourceLimits {
        ResourceLimits {
            mem_mb: Some(1250),
            declared_mem_mb: Some(1000),
            mode: Enforce::Limit,
            ..ResourceLimits::default()
        }
    }

    #[test]
    fn limit_holds_the_job_at_the_ceiling_and_kill_above_it() {
        let mut limits = limit_limits();
        assert_eq!(os_memory_limit_mb(&limits), Some(1250));
        limits.mode = Enforce::Kill;
        assert_eq!(os_memory_limit_mb(&limits), Some(1375));
        limits.mode = Enforce::Warn;
        assert_eq!(os_memory_limit_mb(&limits), None);
        limits.mode = Enforce::Off;
        assert_eq!(os_memory_limit_mb(&limits), None);
        let mut none = limit_limits();
        none.mem_mb = None;
        assert_eq!(os_memory_limit_mb(&none), None);
    }

    #[test]
    fn the_watcher_warns_in_limit_mode_and_never_kills() {
        let mut m = meter(Enforce::Limit);
        assert_eq!(m.observe(1250), Verdict::Within);
        let Verdict::Warn(ex) = m.observe(1500) else {
            panic!("expected a warning");
        };
        assert_eq!((ex.limit_mb, ex.peak_mb), (1250, 1500));
        assert_eq!(m.observe(9000), Verdict::Within);
        let mut disk = Meter::new(LimitedResource::Disk, Some(10), None, Enforce::Limit);
        assert!(matches!(disk.observe(11), Verdict::Warn(_)));
        assert_eq!(disk.observe(50), Verdict::Within);
    }

    #[test]
    fn a_failure_at_the_ceiling_in_limit_mode_is_an_excess() {
        let ex = limit_exceeded(&limit_limits(), 3, Some(1240)).unwrap();
        assert_eq!(ex.resource, LimitedResource::Memory);
        assert_eq!(
            (ex.limit_mb, ex.declared_mb, ex.peak_mb),
            (1250, Some(1000), 1240)
        );
        assert_eq!(
            limit_exceeded(&limit_limits(), 137, Some(1400)).map(|e| e.peak_mb),
            Some(1400)
        );
        assert!(limit_exceeded(&limit_limits(), 3, Some(1125)).is_some());
        assert!(limit_exceeded(&limit_limits(), 3, Some(1124)).is_none());
        assert!(limit_exceeded(&limit_limits(), 0, Some(1250)).is_none());
        assert!(limit_exceeded(&limit_limits(), 3, None).is_none());
        for mode in [Enforce::Off, Enforce::Warn, Enforce::Kill] {
            let mut other = limit_limits();
            other.mode = mode;
            assert!(limit_exceeded(&other, 3, Some(1250)).is_none(), "{mode:?}");
        }
        let mut none = limit_limits();
        none.mem_mb = None;
        assert!(limit_exceeded(&none, 3, Some(1250)).is_none());
    }

    #[test]
    fn the_scope_properties_follow_the_mode_and_the_controllers() {
        let all: Vec<String> = ["memory", "cpu", "pids"].map(String::from).to_vec();
        let mut limits = ResourceLimits {
            mem_mb: Some(1000),
            cpu_millis: Some(4000),
            procs: Some(64),
            mode: Enforce::Kill,
            ..ResourceLimits::default()
        };
        assert_eq!(
            scope_properties(&limits, &all),
            [
                "MemoryMax=1153433600",
                "MemorySwapMax=0",
                "OOMPolicy=continue",
                "CPUQuota=400%",
                "TasksMax=64"
            ]
        );
        assert_eq!(
            scope_properties(&limits, &["pids".to_string()]),
            ["TasksMax=64"]
        );
        limits.mode = Enforce::Limit;
        assert_eq!(
            scope_properties(&limits, &all),
            [
                "MemoryHigh=1048576000",
                "MemoryMax=1153433600",
                "OOMPolicy=continue",
                "CPUQuota=400%",
                "TasksMax=64"
            ]
        );
        assert_eq!(
            scope_properties(&limits, &["cpu".to_string(), "pids".to_string()]),
            ["CPUQuota=400%", "TasksMax=64"]
        );
        limits.mode = Enforce::Warn;
        assert_eq!(
            scope_properties(&limits, &all),
            ["CPUQuota=400%", "TasksMax=64"]
        );
        limits.mode = Enforce::Off;
        assert!(scope_properties(&limits, &all).is_empty());
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
        assert_eq!(
            held_line(&ex),
            "localrunner: failed at the memory ceiling: declared mem_mb 1000 MB, ceiling 1250 MB, peak 1500 MB\n"
        );
        let disk = Exceeded {
            resource: LimitedResource::Disk,
            declared_mb: None,
            ..ex
        };
        assert!(warn_line(&disk).starts_with("localrunner: disk peak 1500 MB exceeds the ceiling"));
    }
}
