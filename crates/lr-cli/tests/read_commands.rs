use clap::Parser;
use lr_cli::commands::history_cmd::{history_filter, history_lines, newest_first, show_lines};
use lr_cli::commands::log_cmd::LogPrinter;
use lr_cli::commands::read_cmd::{runner_lines, stats_lines};
use lr_cli::commands::timefmt::{format_ms, format_time, parse_since, Since};
use lr_cli::commands::{Cli, Command, HistoryAction};
use lr_core::{JobId, JobState, StepState, Timestamp};
use lr_proto::{
    HistoryFilter, JobDetail, JobSummary, ProfileStatsView, RunnerInfo, SpreadView, StatsView,
    StepView, WireCapabilities, WireSource,
};
use std::collections::{BTreeMap, BTreeSet};

const H: u64 = 3_600_000;

fn summary(job: &str, queued: u64) -> JobSummary {
    JobSummary {
        job: JobId(job.into()),
        client_label: "dev".into(),
        state: JobState::Passed,
        platform: Some("linux".into()),
        runner: Some("r1".into()),
        queued_at: Timestamp(queued),
        started_at: Some(Timestamp(queued + 1000)),
        finished_at: Some(Timestamp(queued + 63_000)),
        exit_code: Some(0),
        reason: None,
        project: Some("owner/repo".into()),
        label: Some("agent-1".into()),
        profile: Some("unit".into()),
        source: Some(WireSource::Session),
    }
}

fn masked(job: &str, queued: u64) -> JobSummary {
    JobSummary {
        project: None,
        label: None,
        profile: None,
        source: None,
        client_label: "other-team".into(),
        ..summary(job, queued)
    }
}

#[test]
fn times_print_in_utc_and_since_takes_an_age_or_a_date() {
    assert_eq!(format_time(Timestamp(0)), "1970-01-01 00:00:00Z");
    assert_eq!(
        format_time(Timestamp(1_700_000_000_000)),
        "2023-11-14 22:13:20Z"
    );
    assert_eq!(format_ms(850), "850ms");
    assert_eq!(format_ms(12_400), "12.4s");
    assert_eq!(format_ms(185_000), "3m05s");
    assert_eq!(format_ms(2 * H + 7 * 60_000), "2h07m");
    assert_eq!(parse_since("90s"), Ok(Since::Ago(90_000)));
    assert_eq!(parse_since("30m"), Ok(Since::Ago(1_800_000)));
    assert_eq!(parse_since("2h"), Ok(Since::Ago(2 * H)));
    assert_eq!(parse_since("7d"), Ok(Since::Ago(7 * 24 * H)));
    let Ok(Since::At(t)) = parse_since("2026-11-01") else {
        panic!("a date");
    };
    assert_eq!(format_time(t), "2026-11-01 00:00:00Z");
    assert_eq!(
        Since::Ago(H).resolve(Timestamp(5 * H)),
        Timestamp(4 * H),
        "an age counts back from now"
    );
    for bad in [
        "",
        "d",
        "30",
        "30x",
        "-1d",
        "1.5h",
        "2026-02-30",
        "2026-1-01",
        "2026-13-01",
    ] {
        let err = parse_since(bad).expect_err(bad);
        assert!(
            err.contains("2h") && err.contains("2026-11-01"),
            "{bad}: {err}"
        );
    }
}

fn parse(args: &[&str]) -> Command {
    let mut argv = vec!["localrunner"];
    argv.extend_from_slice(args);
    Cli::try_parse_from(argv).expect("parses").command
}

#[test]
fn history_flags_become_the_brokers_filter() {
    let Command::History(h) = parse(&[
        "history",
        "--project",
        "p",
        "--source",
        "github",
        "--as",
        "a",
        "--profile",
        "unit",
        "--result",
        "failed",
        "--since",
        "2h",
        "--limit",
        "5",
        "--platform",
        "linux",
        "--runner",
        "r1",
        "--client",
        "laptop",
    ]) else {
        panic!("history");
    };
    assert!(h.action.is_none());
    let now = Timestamp(10 * H);
    assert_eq!(
        history_filter(&h.filters, now),
        HistoryFilter {
            client: Some("laptop".into()),
            project: Some("p".into()),
            source: Some(WireSource::Github),
            label: Some("a".into()),
            profile: Some("unit".into()),
            state: Some(JobState::Failed),
            platform: Some("linux".into()),
            runner: Some("r1".into()),
            since: Some(Timestamp(8 * H)),
            active_only: false,
            limit: Some(5),
            cursor: None,
            mine: false,
        }
    );
    let Command::History(own) = parse(&["history"]) else {
        panic!("history");
    };
    let f = history_filter(&own.filters, now);
    assert!(f.mine, "by default only this client's runs");
    assert_eq!(f.client, None);
    let Command::History(all) = parse(&["history", "--all"]) else {
        panic!("history");
    };
    assert!(!history_filter(&all.filters, now).mine);
    let Command::History(show) = parse(&["history", "show", "j1", "--json"]) else {
        panic!("history");
    };
    let Some(HistoryAction::Show(s)) = show.action else {
        panic!("show");
    };
    assert_eq!((s.job.as_str(), s.json), ("j1", true));
    let err = Cli::try_parse_from(["localrunner", "history", "--all", "--client", "x"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("--client"), "{err}");
    let err = Cli::try_parse_from(["localrunner", "history", "--since", "soon"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("2h"), "{err}");
}

#[test]
fn history_lists_newest_first_across_brokers_and_hidden_fields_stay_hidden() {
    let rows = newest_first(
        vec![
            ("local".into(), vec![summary("j3", 3 * H), summary("j1", H)]),
            ("office".into(), vec![masked("o2", 2 * H)]),
        ],
        10,
    );
    let ids: Vec<&str> = rows.iter().map(|(_, s)| s.job.0.as_str()).collect();
    assert_eq!(ids, ["j3", "o2", "j1"]);
    assert_eq!(
        newest_first(
            vec![("local".into(), vec![summary("a", 1), summary("b", 2)])],
            1
        )
        .len(),
        1
    );
    let lines = history_lines(&rows);
    assert_eq!(lines.len(), 4, "a header and one line per run: {lines:?}");
    assert!(
        lines[1].contains("j3") && lines[1].contains("owner/repo") && lines[1].contains("local")
    );
    assert!(
        lines[1].contains("passed") && lines[1].contains("1m02s"),
        "{}",
        lines[1]
    );
    let other = &lines[2];
    assert!(
        other.contains("o2") && other.contains("other-team"),
        "{other}"
    );
    assert!(
        !other.contains("owner/repo") && !other.contains("agent-1"),
        "{other}"
    );
}

#[test]
fn history_show_prints_the_card_with_steps_and_no_secret_values() {
    let detail = JobDetail {
        summary: summary("j1", H),
        note: Some("why".into()),
        branch: Some("main".into()),
        cwd: Some("/work/repo".into()),
        commit: None,
        os_user: Some("alex".into()),
        source: Some(WireSource::Session),
        github_run_url: None,
        steps: vec![StepView {
            ord: 0,
            name: "build".into(),
            state: StepState::Passed,
            exit_code: Some(0),
            started_at: Some(Timestamp(H + 1000)),
            finished_at: Some(Timestamp(H + 11_000)),
            timeout_s: 600,
        }],
        secrets: vec!["registry_auth".into()],
        cancelled_by: None,
        wait_reason: None,
    };
    let text = show_lines("local", &detail).join("\n");
    for needed in [
        "j1",
        "owner/repo",
        "agent-1",
        "why",
        "main",
        "/work/repo",
        "build",
        "10.0s",
        "registry_auth",
        "local",
    ] {
        assert!(text.contains(needed), "{needed} missing:\n{text}");
    }
}

#[test]
fn log_printer_masks_secrets_even_when_a_chunk_splits_them() {
    let mut p = LogPrinter::default();
    assert_eq!(p.push("step one\napi_token=abc"), "step one\n");
    let out = p.push("def123\nkey lrj1.ABCDEFGHIJKLMNOP done\ntail");
    assert!(!out.contains("abc") && !out.contains("def123"), "{out}");
    assert!(!out.contains("ABCDEFGHIJKLMNOP"), "{out}");
    assert!(out.contains(lr_obs::MASK), "{out}");
    assert_eq!(p.finish(), "tail");
    assert_eq!(p.finish(), "");
}

#[test]
fn stats_and_runners_print_per_broker() {
    let stats = StatsView {
        profiles: vec![ProfileStatsView {
            profile: "unit".into(),
            runs: 4,
            passed: 3,
            failed: 1,
            timeouts: 0,
            cancelled: 0,
            lost: 0,
            wait_ms: SpreadView {
                median: Some(1500),
                p95: Some(4000),
            },
            duration_ms: SpreadView {
                median: Some(62_000),
                p95: None,
            },
            steps: Vec::new(),
        }],
        clients: Vec::new(),
    };
    let text = stats_lines(&[("local".into(), stats)]).join("\n");
    for needed in ["local", "unit", "4", "75%", "1.5s", "4.0s", "1m02s"] {
        assert!(text.contains(needed), "{needed}:\n{text}");
    }
    let runner = RunnerInfo {
        name: "r1".into(),
        online: false,
        caps: WireCapabilities {
            platforms: BTreeSet::from(["linux".to_string()]),
            mem_mb: 4096,
            cpu: 4,
            disk_mb: 1000,
            programs: BTreeSet::from(["git".to_string()]),
            tags: BTreeSet::from(["gpu".to_string()]),
            max_job_timeout_s: None,
            services: BTreeMap::new(),
            machine: None,
            accept_min_mem_mb: None,
            cache_max_mb: None,
            cache_used_mb: None,
        },
        last_seen: Some(Timestamp(1_700_000_000_000)),
        free_mem_mb: 1024,
        free_cpu: 2,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: vec!["program docker".into()],
    };
    let text = runner_lines(&[("office".into(), vec![runner])]).join("\n");
    for needed in [
        "office",
        "r1",
        "offline",
        "linux",
        "1024/4096",
        "2/4",
        "gpu",
        "git",
        "program docker",
        "2023-11-14",
    ] {
        assert!(text.contains(needed), "{needed}:\n{text}");
    }
}

#[test]
fn read_commands_parse_their_flags() {
    let Command::Log(l) = parse(&["log", "j1", "--step", "build", "--tail", "20", "-f"]) else {
        panic!("log");
    };
    assert_eq!(
        (l.job.as_str(), l.step.as_deref(), l.tail, l.follow),
        ("j1", Some("build"), Some(20), true)
    );
    let Command::Stats(s) = parse(&[
        "stats",
        "--project",
        "p",
        "--profile",
        "unit",
        "--client",
        "c",
        "--json",
    ]) else {
        panic!("stats");
    };
    assert_eq!(
        (
            s.project.as_deref(),
            s.profile.as_deref(),
            s.client.as_deref(),
            s.json
        ),
        (Some("p"), Some("unit"), Some("c"), true)
    );
    let Command::Runners(r) = parse(&["runners", "--broker", "office"]) else {
        panic!("runners");
    };
    assert_eq!(r.broker.as_deref(), Some("office"));
    let Command::Explain(e) = parse(&["explain", "j2", "--json"]) else {
        panic!("explain");
    };
    assert_eq!((e.job.as_str(), e.json), ("j2", true));
    let Command::Status { html, .. } = parse(&["status", "--html"]) else {
        panic!("status");
    };
    assert_eq!(html.html, Some(None));
    assert!(!html.history && !html.with_logs);
    let Command::Status { html, .. } =
        parse(&["status", "--html", "out.html", "--history", "--with-logs"])
    else {
        panic!("status");
    };
    assert_eq!(html.html, Some(Some("out.html".into())));
    assert!(html.history && html.with_logs);
    assert!(
        Cli::try_parse_from(["localrunner", "status", "--with-logs"]).is_err(),
        "--with-logs needs --html"
    );
    let Command::Up { role } = parse(&["up", "hub"]) else {
        panic!("up");
    };
    assert_eq!(role, Some(lr_cli::commands::RoleArg::Hub));
}

#[test]
fn a_snapshot_asks_logs_of_running_and_finished_jobs_on_their_broker() {
    use lr_cli::commands::snapshot_cmd::{default_path, log_targets};
    use lr_cli::dashboard::detail_of;
    use lr_proto::{DashboardJob, DashboardList, DashboardSnapshot};
    let row = |job: &str, broker: Option<&str>, list: DashboardList| DashboardJob {
        broker: broker.map(String::from),
        list,
        position: None,
        detail: detail_of(summary(job, 0), Vec::new(), None),
        last_lines: Vec::new(),
        stale: false,
    };
    let snap = DashboardSnapshot {
        taken_at: Timestamp(0),
        brokers: Vec::new(),
        jobs: vec![
            row("run", Some("b1"), DashboardList::Running),
            row("queued", Some("b1"), DashboardList::Queued),
            row("outbox", None, DashboardList::Outbox),
            row("done", Some("b2"), DashboardList::History),
        ],
        runners: Vec::new(),
        stats: StatsView {
            profiles: Vec::new(),
            clients: Vec::new(),
        },
    };
    assert_eq!(
        log_targets(&snap),
        vec![
            ("b1".to_string(), JobId("run".into())),
            ("b2".to_string(), JobId("done".into()))
        ]
    );
    assert!(default_path().ends_with("temp/runner/queue.html"));
}
