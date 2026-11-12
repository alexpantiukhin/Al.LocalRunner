use lr_cli::commands::history_cmd::{history_lines, show_lines};
use lr_cli::commands::jobs::mine_line;
use lr_cli::commands::read_cmd::runner_lines;
use lr_cli::commands::view::{add_ages, status_json};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{
    JobDetail, JobSummary, PoolLoad, ReasonCode, RunnerInfo, StatusView, WireCapabilities,
    WireSource,
};
use std::collections::{BTreeMap, BTreeSet};

fn summary(peaks: Option<(u64, u64)>) -> JobSummary {
    JobSummary {
        job: JobId("j1".into()),
        client_label: "dev".into(),
        state: JobState::Failed,
        platform: None,
        runner: Some("r1".into()),
        queued_at: Timestamp(1000),
        started_at: Some(Timestamp(2000)),
        finished_at: Some(Timestamp(9000)),
        exit_code: None,
        reason: Some(ReasonCode::ResourceExceeded),
        project: Some("owner/repo".into()),
        label: Some("agent".into()),
        profile: Some("unit".into()),
        source: Some(WireSource::Session),
        note: None,
        branch: None,
        cwd: None,
        timeout_s: None,
        github_run_url: None,
        peak_mem_mb: peaks.map(|p| p.0),
        peak_disk_mb: peaks.map(|p| p.1),
    }
}

fn detail(s: JobSummary) -> JobDetail {
    JobDetail {
        summary: s,
        note: None,
        branch: None,
        cwd: None,
        commit: None,
        os_user: None,
        source: None,
        github_run_url: None,
        steps: Vec::new(),
        secrets: Vec::new(),
        cancelled_by: None,
        wait_reason: None,
    }
}

#[test]
fn history_has_peak_columns_with_dashes_for_unknown_values() {
    let rows = vec![
        ("local".to_string(), summary(Some((900, 40)))),
        ("local".to_string(), summary(None)),
    ];
    let lines = history_lines(&rows);
    assert!(lines[0].contains("RUNNER\tPEAK_MEM_MB\tPEAK_DISK_MB\tBROKER"));
    assert!(lines[1].ends_with("r1\t900\t40\tlocal"), "{}", lines[1]);
    assert!(lines[2].ends_with("r1\t-\t-\tlocal"), "{}", lines[2]);
}

#[test]
fn the_card_shows_the_reason_and_peaks_only_when_known() {
    let text = show_lines("local", &detail(summary(Some((900, 40))))).join("\n");
    assert!(text.contains("resource_exceeded"), "{text}");
    assert!(text.contains("peak mem  900 MB"), "{text}");
    assert!(text.contains("peak disk 40 MB"), "{text}");
    let bare = show_lines("local", &detail(summary(None))).join("\n");
    assert!(!bare.contains("peak"), "{bare}");
}

#[test]
fn jobs_line_adds_peaks_only_when_known() {
    assert!(mine_line(&summary(Some((900, 40)))).ends_with("\tpeak mem 900 MB, disk 40 MB"));
    assert!(!mine_line(&summary(None)).contains("peak"));
}

#[test]
fn status_json_carries_peaks_of_recent_jobs() {
    let view = StatusView {
        running: Vec::new(),
        queue: Vec::new(),
        recent: vec![summary(Some((900, 40))), summary(None)],
        pool: PoolLoad {
            used_mem_mb: 0,
            total_mem_mb: 1024,
            used_cpu: 0,
            total_cpu: 1,
        },
        next: None,
    };
    let mut json = status_json(&[("local".to_string(), view)]);
    add_ages(&mut json, Timestamp(10_000));
    let recent = &json["brokers"][0]["recent"];
    assert_eq!(recent[0]["peak_mem_mb"], 900);
    assert_eq!(recent[0]["peak_disk_mb"], 40);
    assert_eq!(recent[0]["reason"], "resource_exceeded");
    assert!(recent[1].get("peak_mem_mb").is_none());
}

#[test]
fn runners_list_shows_enforcement() {
    let caps = |enforcement: Option<&str>| WireCapabilities {
        platforms: BTreeSet::from(["linux".to_string()]),
        mem_mb: 4096,
        cpu: 4,
        disk_mb: 1000,
        programs: BTreeSet::new(),
        tags: BTreeSet::new(),
        max_job_timeout_s: None,
        services: BTreeMap::new(),
        machine: None,
        accept_min_mem_mb: None,
        cache_max_mb: None,
        cache_used_mb: None,
        service_mem_mb: Default::default(),
        commands: Default::default(),
        enforcement: enforcement.map(str::to_string),
        version: None,
    };
    let runner = |name: &str, enforcement: Option<&str>| RunnerInfo {
        name: name.into(),
        online: true,
        caps: caps(enforcement),
        last_seen: None,
        free_mem_mb: 1,
        free_cpu: 1,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: Vec::new(),
    };
    let text = runner_lines(&[(
        "office".into(),
        vec![runner("r1", Some("watcher")), runner("r2", None)],
    )])
    .join("\n");
    assert_eq!(text.matches("enforcement").count(), 1, "{text}");
    assert!(text.contains("    enforcement watcher"), "{text}");
}
