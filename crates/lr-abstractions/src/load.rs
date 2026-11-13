//! The real load of the host machine, for admission by foreign load (issue 35, item 4): what
//! memory is free and how busy the CPU is right now, whoever uses it.

/// One reading of the machine load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSample {
    /// Memory the OS can give to a new process, MB.
    pub free_mem_mb: u64,
    /// Share of CPU time busy since the previous reading, 0 to 100.
    pub cpu_busy_percent: u8,
    /// Logical cores of the machine.
    pub cpu_cores: u32,
}

/// Reads the load of the machine the runner works on.
pub trait HostLoad: Send + Sync {
    /// The current load; `None` when it cannot be measured (the platform has no reader, or a
    /// CPU reading needs a previous one that does not exist yet).
    fn sample(&self) -> Option<LoadSample>;
}

/// Cumulative CPU time of the machine, in any one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Time spent idle.
    pub idle: u64,
    /// Time spent in total, idle included.
    pub total: u64,
}

/// Busy share between two readings, 0 to 100; `None` when no time passed between them.
pub fn busy_percent(prev: CpuTimes, now: CpuTimes) -> Option<u8> {
    let total = now.total.checked_sub(prev.total).filter(|t| *t > 0)?;
    let idle = now.idle.saturating_sub(prev.idle).min(total);
    let busy = u128::from(total - idle) * 100;
    Some(u8::try_from(busy.div_ceil(u128::from(total)).min(100)).unwrap_or(100))
}

/// `MemAvailable` of `/proc/meminfo` in MB.
pub fn mem_available_mb(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// What is left under a cgroup v2 memory limit in MB: `memory.max` minus `memory.current`.
/// `None` for `max` (no limit) or text that is not a number; a container started with
/// `mem_limit` has a limit far below the memory of its host or VM.
pub fn cgroup_free_mb(max: &str, current: &str) -> Option<u64> {
    let limit: u64 = max.trim().parse().ok()?;
    let used: u64 = current.trim().parse().ok()?;
    Some(limit.saturating_sub(used) / (1024 * 1024))
}

/// Free memory from the output of `vm_stat` on macOS in MB: free, inactive and speculative
/// pages, which the OS gives to a new process without swapping, times the page size of the
/// header line.
pub fn vm_stat_free_mb(text: &str) -> Option<u64> {
    let header = text.lines().next()?;
    let size: u64 = header
        .split("page size of")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(name))?;
        line.split(':')
            .nth(1)?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let free = pages("Pages free")?;
    let inactive = pages("Pages inactive").unwrap_or(0);
    let speculative = pages("Pages speculative").unwrap_or(0);
    Some((free + inactive + speculative).saturating_mul(size) / (1024 * 1024))
}

/// The busy share of the CPU from the output of `ps -A -o %cpu` (the sum over all processes
/// divided by the logical cores), 0 to 100.
pub fn ps_busy_percent(text: &str, cores: u32) -> Option<u8> {
    let mut sum = 0.0f64;
    let mut any = false;
    for line in text.lines().skip(1) {
        if let Ok(v) = line.trim().replace(',', ".").parse::<f64>() {
            sum += v;
            any = true;
        }
    }
    if !any || cores == 0 {
        return None;
    }
    let share = (sum / f64::from(cores)).clamp(0.0, 100.0).ceil();
    Some(share as u8)
}

/// The aggregate `cpu` line of `/proc/stat` as [`CpuTimes`]: idle and iowait count as idle.
pub fn proc_stat_times(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3].saturating_add(fields.get(4).copied().unwrap_or(0));
    let total = fields
        .iter()
        .take(8)
        .fold(0u64, |a, f| a.saturating_add(*f));
    Some(CpuTimes { idle, total })
}
