use crate::hooks_config::DEFAULT_HEADROOM_PCT;
use crate::settings::RunnerSettings;
use lr_abstractions::{Enforce, Exceeded, LimitedResource, ResourceLimits};
use lr_core::Requirements;
use lr_proto::{WireEnforce, WireLimits};
use std::path::PathBuf;

const MIN_PROCS: u32 = 512;
const PROCS_PER_CPU: u32 = 256;

fn mode(broker: Option<&WireLimits>, settings: &RunnerSettings) -> Enforce {
    settings.enforce.unwrap_or(match broker.map(|l| l.mode) {
        Some(WireEnforce::Warn) => Enforce::Warn,
        Some(WireEnforce::Kill) => Enforce::Kill,
        Some(WireEnforce::Off) | None => Enforce::Off,
    })
}

/// The disk headroom in percent, if the owner or the broker configured one: the owner's
/// `[limits] disk_headroom` wins over the broker's policy.
pub(crate) fn disk_headroom_pct(
    broker: Option<&WireLimits>,
    settings: &RunnerSettings,
) -> Option<u32> {
    settings
        .disk_headroom_pct
        .or(broker.map(|l| l.disk_headroom_pct))
}

fn ceiling(declared: u64, pct: u32) -> u64 {
    declared.saturating_mul(u64::from(pct)).div_ceil(100)
}

/// The resource ceilings of one job. With the mode off (neither the broker nor `runner.toml`
/// turned enforcement on) nothing is limited and only the peak of the disk under `dirs` is
/// measured; otherwise the memory and disk ceilings are the declared value times the headroom,
/// the CPU share is the declared count and the process count a generous multiple of it.
pub(crate) fn job_limits(
    req: &Requirements,
    broker: Option<&WireLimits>,
    settings: &RunnerSettings,
    dirs: Vec<PathBuf>,
) -> ResourceLimits {
    let mode = mode(broker, settings);
    let mem_pct = settings
        .mem_headroom_pct
        .or(broker.map(|l| l.mem_headroom_pct))
        .unwrap_or(DEFAULT_HEADROOM_PCT);
    let disk_pct = disk_headroom_pct(broker, settings).unwrap_or(DEFAULT_HEADROOM_PCT);
    let on = mode != Enforce::Off;
    let declared = |v: u64| (v > 0).then_some(v);
    let measured_dirs = if req.disk_mb > 0 { dirs } else { Vec::new() };
    ResourceLimits {
        mem_mb: declared(req.mem_mb)
            .filter(|_| on)
            .map(|m| ceiling(m, mem_pct)),
        cpu_millis: (req.cpu > 0 && on).then(|| req.cpu.saturating_mul(1000)),
        procs: on.then(|| MIN_PROCS.max(req.cpu.saturating_mul(PROCS_PER_CPU))),
        disk_mb: declared(req.disk_mb)
            .filter(|_| on)
            .map(|d| ceiling(d, disk_pct)),
        disk_dirs: measured_dirs,
        mode,
        declared_mem_mb: declared(req.mem_mb),
        declared_disk_mb: declared(req.disk_mb),
    }
}

/// The reason text of a job killed for exceeding a declared resource.
pub(crate) fn exceeded_detail(e: &Exceeded) -> String {
    let (key, declared) = match e.resource {
        LimitedResource::Memory => ("mem_mb", e.declared_mb.unwrap_or(e.limit_mb)),
        LimitedResource::Disk => ("disk_mb", e.declared_mb.unwrap_or(e.limit_mb)),
    };
    format!(
        "превышен заявленный {key}: заявлено {declared} МБ, пик {} МБ",
        e.peak_mb
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn req(mem: u64, cpu: u32, disk: u64) -> Requirements {
        Requirements {
            mem_mb: mem,
            cpu,
            disk_mb: disk,
            platform: None,
            timeout_s: 60,
            programs: Vec::new(),
            tags: Default::default(),
            exclusive: Default::default(),
            same_machine: None,
            services: Vec::new(),
            secrets: Default::default(),
            cache: Vec::new(),
            max_zone: None,
            commands: Default::default(),
        }
    }

    fn broker(mode: WireEnforce, mem: u32, disk: u32) -> WireLimits {
        WireLimits {
            mode,
            mem_headroom_pct: mem,
            disk_headroom_pct: disk,
        }
    }

    fn settings() -> RunnerSettings {
        RunnerSettings::new("r", Path::new("/s").to_path_buf())
    }

    fn dirs() -> Vec<PathBuf> {
        vec![PathBuf::from("/w"), PathBuf::from("/t")]
    }

    #[test]
    fn nothing_configured_limits_nothing_but_measures_disk() {
        let l = job_limits(&req(4000, 4, 2000), None, &settings(), dirs());
        assert_eq!(l.mode, Enforce::Off);
        assert_eq!(
            (l.mem_mb, l.cpu_millis, l.procs, l.disk_mb),
            (None, None, None, None)
        );
        assert_eq!(l.disk_dirs, dirs());
        assert_eq!(l.declared_mem_mb, Some(4000));
    }

    #[test]
    fn broker_policy_sets_ceilings() {
        let b = broker(WireEnforce::Kill, 125, 150);
        let l = job_limits(&req(4000, 4, 2000), Some(&b), &settings(), dirs());
        assert_eq!(l.mode, Enforce::Kill);
        assert_eq!(l.mem_mb, Some(5000));
        assert_eq!(l.disk_mb, Some(3000));
        assert_eq!(l.cpu_millis, Some(4000));
        assert_eq!(l.procs, Some(1024));
        assert_eq!(l.declared_disk_mb, Some(2000));
    }

    #[test]
    fn runner_toml_replaces_the_broker_mode_and_headroom() {
        let b = broker(WireEnforce::Kill, 125, 150);
        let mut s = settings();
        s.enforce = Some(Enforce::Off);
        let off = job_limits(&req(4000, 1, 0), Some(&b), &s, dirs());
        assert_eq!(off.mode, Enforce::Off);
        assert_eq!(off.mem_mb, None);
        s.enforce = Some(Enforce::Warn);
        s.mem_headroom_pct = Some(200);
        let warn = job_limits(&req(4000, 1, 0), Some(&b), &s, dirs());
        assert_eq!((warn.mode, warn.mem_mb), (Enforce::Warn, Some(8000)));
        assert_eq!(warn.procs, Some(512));
        assert_eq!(warn.disk_mb, None);
        assert!(warn.disk_dirs.is_empty());
    }

    #[test]
    fn owner_enforcement_without_headroom_uses_the_default() {
        let mut s = settings();
        s.enforce = Some(Enforce::Kill);
        let l = job_limits(&req(1000, 0, 100), None, &s, dirs());
        assert_eq!(
            (l.mem_mb, l.disk_mb, l.cpu_millis),
            (Some(1250), Some(125), None)
        );
    }

    #[test]
    fn disk_headroom_prefers_the_owner() {
        let b = broker(WireEnforce::Off, 125, 150);
        let mut s = settings();
        assert_eq!(disk_headroom_pct(None, &s), None);
        assert_eq!(disk_headroom_pct(Some(&b), &s), Some(150));
        s.disk_headroom_pct = Some(110);
        assert_eq!(disk_headroom_pct(Some(&b), &s), Some(110));
    }

    #[test]
    fn exceeded_text_names_declared_and_peak() {
        let e = Exceeded {
            resource: LimitedResource::Memory,
            limit_mb: 5000,
            declared_mb: Some(4000),
            peak_mb: 5200,
        };
        assert_eq!(
            exceeded_detail(&e),
            "превышен заявленный mem_mb: заявлено 4000 МБ, пик 5200 МБ"
        );
        let d = Exceeded {
            resource: LimitedResource::Disk,
            declared_mb: None,
            ..e
        };
        assert!(exceeded_detail(&d).starts_with("превышен заявленный disk_mb: заявлено 5000"));
    }
}
