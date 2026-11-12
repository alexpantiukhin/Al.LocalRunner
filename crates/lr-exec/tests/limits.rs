mod common;

use common::{events_text, job, step, workdir, Recorder};
use lr_abstractions::{
    stop_signal, Enforce, JobEvent, JobOutcome, LimitedResource, ProcessRunner, ResourceLimits,
};
use lr_core::{JobState, StepState};
use lr_exec::{ExecRunner, SystemClock};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn allocate(chunks: u32, then_sleep_s: u32) -> String {
    format!(
        "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -Command \"$a=@(); for($i=0;$i -lt {chunks};$i++){{ $a += ,(New-Object byte[] 4MB); Start-Sleep -Milliseconds 100 }}; Start-Sleep {then_sleep_s}\""
    )
}

#[cfg(unix)]
fn allocate(chunks: u32, then_sleep_s: u32) -> String {
    format!(
        "x=$(head -c {} /dev/zero | tr '\\0' a); sleep {then_sleep_s}",
        u64::from(chunks) * 4_000_000
    )
}

#[cfg(windows)]
fn write_big_file(mb: u32, then_sleep_s: u32) -> String {
    format!(
        "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -Command \"[IO.File]::WriteAllBytes('big.bin', (New-Object byte[] {mb}MB)); Start-Sleep {then_sleep_s}\""
    )
}

#[cfg(unix)]
fn write_big_file(mb: u32, then_sleep_s: u32) -> String {
    format!(
        "head -c {} /dev/zero > big.bin; sleep {then_sleep_s}",
        u64::from(mb) * 1_048_576
    )
}

async fn run(
    name: &str,
    command: &str,
    limits: ResourceLimits,
    timeout_s: u64,
) -> (JobOutcome, Vec<JobEvent>, Duration) {
    let mut spec = job(name, timeout_s, vec![step("work", command, timeout_s)]);
    spec.cwd = workdir(name);
    let _ = std::fs::remove_file(spec.cwd.join("big.bin"));
    spec.limits = limits;
    let runner = ExecRunner::new(Arc::new(SystemClock));
    let out = Recorder::new();
    let (_handle, stop) = stop_signal();
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(90),
        runner.run_job(spec, out.as_ref(), stop),
    )
    .await
    .expect("run_job did not return");
    (outcome, out.events(), started.elapsed())
}

fn mem_limits(mode: Enforce) -> ResourceLimits {
    ResourceLimits {
        mem_mb: Some(150),
        declared_mem_mb: Some(100),
        mode,
        ..ResourceLimits::default()
    }
}

#[tokio::test]
async fn kill_mode_ends_a_job_over_the_memory_ceiling() {
    let (outcome, events, took) = run(
        "mem_kill",
        &allocate(100, 60),
        mem_limits(Enforce::Kill),
        120,
    )
    .await;
    let ex = outcome.exceeded.expect("exceeded is set");
    assert_eq!(ex.resource, LimitedResource::Memory);
    assert_eq!(ex.limit_mb, 150);
    assert_eq!(ex.declared_mb, Some(100));
    assert!(ex.peak_mb > 150);
    assert_eq!(outcome.state, JobState::Failed);
    assert_eq!(outcome.steps[0].state, StepState::Failed);
    assert_eq!(outcome.steps[0].exit_code, None);
    assert!(outcome.usage.peak_mem_mb.unwrap() > 150);
    assert!(took < Duration::from_secs(45), "took {took:?}");
    let text = events_text(&events);
    assert!(
        text.contains("localrunner: killed: declared mem_mb exceeded: declared 100 MB, peak"),
        "{text}"
    );
}

#[tokio::test]
async fn warn_mode_keeps_the_job_running_and_writes_one_line() {
    let (outcome, events, _) = run(
        "mem_warn",
        &allocate(100, 2),
        mem_limits(Enforce::Warn),
        120,
    )
    .await;
    assert_eq!(outcome.state, JobState::Passed, "{}", events_text(&events));
    assert!(outcome.exceeded.is_none());
    assert!(outcome.usage.peak_mem_mb.unwrap() > 150);
    let text = events_text(&events);
    assert_eq!(
        text.matches("exceeds the declared mem_mb 100 MB").count(),
        1
    );
    assert!(text.contains("(ceiling 150 MB)"));
}

#[tokio::test]
async fn off_mode_records_the_peak_and_limits_nothing() {
    let (outcome, events, _) =
        run("mem_off", &allocate(100, 1), mem_limits(Enforce::Off), 120).await;
    assert_eq!(outcome.state, JobState::Passed, "{}", events_text(&events));
    assert!(outcome.exceeded.is_none());
    assert!(outcome.usage.peak_mem_mb.unwrap() > 150);
    assert!(!events_text(&events).contains("exceeds"));
}

#[tokio::test]
async fn no_limits_changes_nothing_but_the_peak_is_recorded() {
    let (outcome, _, _) = run("mem_none", "echo hi", ResourceLimits::default(), 60).await;
    assert_eq!(outcome.state, JobState::Passed);
    assert!(outcome.exceeded.is_none());
}

#[tokio::test]
async fn kill_mode_ends_a_job_over_the_disk_ceiling() {
    let limits = ResourceLimits {
        disk_mb: Some(5),
        declared_disk_mb: Some(4),
        disk_dirs: vec![workdir("disk_kill")],
        mode: Enforce::Kill,
        ..ResourceLimits::default()
    };
    let (outcome, events, took) = run("disk_kill", &write_big_file(20, 60), limits, 120).await;
    let ex = outcome.exceeded.expect("exceeded is set");
    assert_eq!(ex.resource, LimitedResource::Disk);
    assert!(ex.peak_mb > ex.limit_mb);
    assert_eq!(outcome.state, JobState::Failed);
    assert!(outcome.usage.peak_disk_mb.unwrap() > 5);
    assert!(took < Duration::from_secs(45), "took {took:?}");
    assert!(events_text(&events).contains("killed: declared disk_mb exceeded: declared 4 MB"));
}

#[cfg(windows)]
#[tokio::test]
async fn the_process_limit_stops_a_second_process() {
    let command = "%SystemRoot%\\System32\\ping.exe -n 2 127.0.0.1 >nul";
    let free = ResourceLimits {
        procs: Some(1),
        mode: Enforce::Off,
        ..ResourceLimits::default()
    };
    let (outcome, _, _) = run("procs_off", command, free, 60).await;
    assert_eq!(outcome.state, JobState::Passed);
    let capped = ResourceLimits {
        procs: Some(1),
        mode: Enforce::Warn,
        ..ResourceLimits::default()
    };
    let (outcome, _, _) = run("procs_on", command, capped, 60).await;
    assert_eq!(outcome.state, JobState::Failed);
    assert!(outcome.exceeded.is_none());
}

#[cfg(windows)]
#[tokio::test]
async fn a_cpu_cap_still_lets_the_job_finish() {
    let limits = ResourceLimits {
        cpu_millis: Some(1000),
        mode: Enforce::Warn,
        ..ResourceLimits::default()
    };
    let (outcome, _, _) = run("cpu_cap", "echo ok", limits, 60).await;
    assert_eq!(outcome.state, JobState::Passed);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_fast_allocation_killed_by_the_os_backstop_is_still_an_excess() {
    if lr_exec::enforcement() != lr_exec::Enforcement::Os {
        return;
    }
    let command = "x=$(head -c 900000000 /dev/zero | tr '\\0' a); sleep 5";
    let (outcome, events, took) = run("mem_oom", command, mem_limits(Enforce::Kill), 120).await;
    let text = events_text(&events);
    let ex = outcome.exceeded.expect(&text);
    assert_eq!(ex.resource, LimitedResource::Memory);
    assert_eq!((ex.limit_mb, ex.declared_mb), (150, Some(100)));
    assert!(ex.peak_mb >= 165);
    assert_eq!(outcome.state, JobState::Failed);
    assert!(outcome.usage.peak_mem_mb.unwrap() >= 165);
    assert!(took < Duration::from_secs(45), "took {took:?}");
    assert!(
        text.contains("localrunner: killed: declared mem_mb exceeded: declared 100 MB"),
        "{text}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_external_sigkill_is_not_an_excess() {
    if lr_exec::enforcement() != lr_exec::Enforcement::Os {
        return;
    }
    let (outcome, _, _) = run("mem_sigkill", "kill -9 $$", mem_limits(Enforce::Kill), 60).await;
    assert!(outcome.exceeded.is_none());
    assert_eq!(outcome.state, JobState::Failed);
}
