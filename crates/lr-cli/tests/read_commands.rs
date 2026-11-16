use clap::Parser;
use lr_cli::commands::history_cmd::{history_filter, history_lines, newest_first, show_lines};
use lr_cli::commands::log_cmd::LogPrinter;
use lr_cli::commands::read_cmd::{runner_lines, stats_lines};
use lr_cli::commands::timefmt::{format_ms, format_time, parse_since, Since};
use lr_cli::commands::view::{add_runners, job_lines, not_serving_lines, runners_of};
use lr_cli::commands::{Cli, Command, HistoryAction, OutboxAction, RunnerAction};
use lr_core::{JobId, JobState, StepState, Timestamp};
use lr_proto::{
    HistoryFilter, JobDetail, JobSummary, ProfileStatsView, ReadReply, RunnerInfo, RunnerLiveness,
    SpreadView, StatsView, StepView, WireCapabilities, WireSource,
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
        note: None,
        branch: None,
        cwd: None,
        timeout_s: None,
        github_run_url: None,
        peak_mem_mb: None,
        peak_disk_mb: None,
        labels: Vec::new(),
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
        lines[0].contains("PROFILE	PROJECT	SOURCE	LABEL	"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("	owner/repo	session	agent-1	"),
        "{}",
        lines[1]
    );
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
        online: true,
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
            service_mem_mb: Default::default(),
            commands: Default::default(),
            enforcement: None,
            version: None,
        },
        last_seen: Some(Timestamp(1_700_000_000_000)),
        free_mem_mb: 1024,
        free_cpu: 2,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: vec!["program docker".into()],
        liveness: Some(RunnerLiveness::Stale),
        silent_for_s: Some(40),
    };
    let text = runner_lines(&[("office".into(), vec![runner.clone()])]).join("\n");
    for needed in [
        "office",
        "r1",
        "stale (silent 40 s)",
        "linux",
        "memory 4096 MB, free in the pool 1024 MB",
        "cpu 4, free in the pool 2",
        "gpu",
        "git",
        "program docker",
        "2023-11-14",
    ] {
        assert!(text.contains(needed), "{needed}:\n{text}");
    }
    let mut gone = runner.clone();
    gone.name = "r2".into();
    gone.online = false;
    gone.liveness = Some(RunnerLiveness::Offline);
    let mut old = runner.clone();
    old.name = "r3".into();
    old.liveness = None;
    old.silent_for_s = None;
    let text = runner_lines(&[("office".into(), vec![gone, old])]).join("\n");
    assert!(text.contains("r2\toffline"), "{text}");
    assert!(text.contains("r3\tonline"), "{text}");
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
        wait_code: None,
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

#[test]
fn history_prints_a_dash_for_an_empty_client_label() {
    let rows = vec![(
        "local".to_string(),
        JobSummary {
            client_label: String::new(),
            ..summary("j1", H)
        },
    )];
    let lines = history_lines(&rows);
    let cols: Vec<&str> = lines[1].split('\t').collect();
    assert_eq!(cols[8], "-", "{}", lines[1]);
    assert!(cols.iter().all(|c| !c.is_empty()), "{}", lines[1]);
}

#[test]
fn a_cancelled_run_with_a_rejection_reason_is_shown_and_filtered_as_rejected() {
    use lr_cli::commands::history_cmd::{is_rejected, shown_state};
    use lr_proto::{ReasonCode, RejectReason};
    let mut refused = summary("j9", H);
    refused.state = lr_core::JobState::Cancelled;
    refused.reason = Some(ReasonCode::Rejected(RejectReason::PoolMemory));
    let mut plain = summary("j8", H);
    plain.state = lr_core::JobState::Cancelled;
    assert!(is_rejected(&refused));
    assert!(!is_rejected(&plain));
    assert_eq!(shown_state(&refused), "rejected");
    assert_eq!(shown_state(&plain), "cancelled");
    let lines = history_lines(&[("local".into(), refused)]);
    assert!(lines[1].contains("\trejected\t"), "{}", lines[1]);
    let parsed = Cli::try_parse_from(["localrunner", "history", "--result", "rejected"]);
    assert!(parsed.is_ok());
}

#[test]
fn runner_outbox_drop_takes_a_job_and_an_optional_runner() {
    let Command::Runner {
        action:
            RunnerAction::Outbox {
                action: OutboxAction::Drop { runner, job },
            },
    } = parse(&["runner", "outbox", "drop", "j1"])
    else {
        panic!("not runner outbox drop");
    };
    assert_eq!((runner, job.as_str()), (None, "j1"));
    let Command::Runner {
        action:
            RunnerAction::Outbox {
                action: OutboxAction::Drop { runner, job },
            },
    } = parse(&["runner", "outbox", "drop", "--runner", "r1", "j2"])
    else {
        panic!("not runner outbox drop");
    };
    assert_eq!((runner.as_deref(), job.as_str()), (Some("r1"), "j2"));
}

fn runner_info(name: &str, liveness: RunnerLiveness, silent: u64) -> RunnerInfo {
    RunnerInfo {
        name: name.into(),
        online: liveness != RunnerLiveness::Offline,
        caps: WireCapabilities {
            platforms: BTreeSet::new(),
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
            enforcement: None,
            version: None,
        },
        last_seen: Some(Timestamp(1_700_000_000_000)),
        free_mem_mb: 0,
        free_cpu: 0,
        held_tags: Vec::new(),
        services: Vec::new(),
        unmet: Vec::new(),
        liveness: Some(liveness),
        silent_for_s: Some(silent),
    }
}

#[test]
fn status_with_a_job_id_parses_and_prints_its_state_and_reason() {
    match parse(&["status", "j2", "--broker", "office"]) {
        Command::Status { job, broker, .. } => {
            assert_eq!(job.as_deref(), Some("j2"));
            assert_eq!(broker.as_deref(), Some("office"));
        }
        other => panic!("{other:?}"),
    }
    match parse(&["status"]) {
        Command::Status { job, .. } => assert_eq!(job, None),
        other => panic!("{other:?}"),
    }
    assert!(Cli::try_parse_from(["localrunner", "status", "j2", "--html"]).is_err());
    let mut detail = JobDetail {
        summary: summary("j2", H),
        note: None,
        branch: None,
        cwd: None,
        commit: None,
        os_user: None,
        source: Some(WireSource::Session),
        github_run_url: None,
        steps: Vec::new(),
        secrets: Vec::new(),
        cancelled_by: None,
        wait_reason: Some("раннер занят заказом j1 (3 мин)".into()),
    };
    detail.summary.state = JobState::Queued;
    let text = job_lines(
        "office",
        &JobId("j2".into()),
        "ждёт: раннер занят заказом j1 (3 мин)",
        Some(&detail),
    )
    .join("\n");
    for needed in ["j2", "office", "queued", "раннер занят заказом j1 (3 мин)"]
    {
        assert!(text.contains(needed), "{needed} missing:\n{text}");
    }
    let bare = job_lines("office", &JobId("j2".into()), "ждёт в очереди", None).join("\n");
    assert!(bare.contains("ждёт в очереди"), "{bare}");
}

#[test]
fn status_lists_offline_and_stale_runners_with_last_seen() {
    let per = vec![(
        "office".to_string(),
        vec![
            runner_info("r1", RunnerLiveness::Online, 3),
            runner_info("r2", RunnerLiveness::Stale, 40),
            runner_info("r3", RunnerLiveness::Offline, 7200),
        ],
    )];
    let text = not_serving_lines(&per).join("\n");
    assert!(!text.contains("r1"), "{text}");
    for needed in [
        "office",
        "r2",
        "stale",
        "r3",
        "offline",
        "2023-11-14",
        "2h00m",
    ] {
        assert!(text.contains(needed), "{needed} missing:\n{text}");
    }
    let healthy = vec![(
        "office".to_string(),
        vec![runner_info("r1", RunnerLiveness::Online, 3)],
    )];
    assert!(not_serving_lines(&healthy).is_empty());
}

#[test]
fn status_json_has_a_runners_key() {
    let mut doc = serde_json::json!({ "brokers": [{ "broker": "office" }, { "broker": "old" }] });
    let per = vec![(
        "office".to_string(),
        vec![
            runner_info("r1", RunnerLiveness::Online, 3),
            runner_info("r3", RunnerLiveness::Offline, 7200),
        ],
    )];
    add_runners(&mut doc, &per);
    let runners = doc["brokers"][0]["runners"].as_array().expect("runners");
    assert_eq!(runners.len(), 1);
    assert_eq!(runners[0]["name"], "r3");
    assert_eq!(runners[0]["liveness"], "offline");
    assert_eq!(runners[0]["silent_for_s"], 7200);
    assert_eq!(runners[0]["last_seen"], 1_700_000_000_000u64);
    assert!(doc["brokers"][1].get("runners").is_none());
}

#[test]
fn status_survives_a_broker_that_does_not_know_runners() {
    assert!(runners_of(ReadReply::Unknown).is_none());
    assert!(runners_of(ReadReply::Runners {
        runners: Vec::new()
    })
    .is_some());
    let mut doc = serde_json::json!({ "brokers": [{ "broker": "old" }] });
    add_runners(&mut doc, &[]);
    assert!(doc["brokers"][0].get("runners").is_none());
    assert!(not_serving_lines(&[]).is_empty());
}
