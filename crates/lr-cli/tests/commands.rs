mod common;

use async_trait::async_trait;
use lr_abstractions::fakes::{FakeNetwork, ManualClock};
use lr_abstractions::{Dialer, Frame, Peer, PeerAccess};
use lr_cli::admin::{serve, AdminApi};
use lr_cli::commands::jobs::{missing_label_text, LabelLookup};
use lr_cli::commands::view::{add_ages, status_json, status_text};
use lr_core::{Endpoint, JobId, JobState, Timestamp};
use lr_proto::{
    AdminReply, AdminRequest, IpcChannel, IpcOpen, IpcOpenReply, JobDetail, JobSummary, PoolLoad,
    QueueItem, RunningView, StatusView, WireSource, PROTOCOL_VERSION,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn localrunner(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .env(
            "LOCALRUNNER_STATE_DIR",
            std::env::temp_dir().join("lr-cli-no-state"),
        )
        .output()
        .expect("the binary runs")
}

#[test]
fn submit_without_as_refused() {
    for cmd in ["submit", "run"] {
        let out = localrunner(&[cmd, "unit"]);
        assert!(!out.status.success(), "{cmd} without --as succeeded");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--as"), "{cmd}: {err}");
    }
}

#[test]
fn inside_github_actions_submit_needs_no_as() {
    let out = Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["submit", "unit"])
        .env(
            "LOCALRUNNER_STATE_DIR",
            std::env::temp_dir().join("lr-cli-no-state"),
        )
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "acme/app")
        .env("GITHUB_WORKFLOW", "ci")
        .env("GITHUB_JOB", "test")
        .env("GITHUB_RUN_ID", "42")
        .output()
        .expect("the binary runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("--as"), "{err}");
    assert!(
        err.contains("error: the hub is not running; start it: localrunner up"),
        "{err}"
    );
    assert!(!err.contains("pipe") && !err.contains("os error"), "{err}");
}

#[test]
fn status_without_a_hub_says_so_briefly() {
    let out = localrunner(&["status"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    let note = format!(
        "state directory: {} (from LOCALRUNNER_STATE_DIR)",
        std::env::temp_dir().join("lr-cli-no-state").display()
    );
    assert_eq!(
        err.trim(),
        format!("{note}\nerror: the hub is not running; start it: localrunner up")
    );
}

#[test]
fn the_state_directory_note_goes_to_stderr_of_status_doctor_up_and_down_only() {
    let dir = std::env::temp_dir().join("lr-cli-no-state");
    for cmd in ["status", "doctor"] {
        let out = localrunner(&[cmd]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with(&format!(
                "state directory: {} (from LOCALRUNNER_STATE_DIR)\n",
                dir.display()
            )),
            "{cmd}: {err}"
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("(from LOCALRUNNER_STATE_DIR)"));
    }
    let out = localrunner(&["jobs", "--as", "dev"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("state directory:"));
    let out = Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["status", "--state-dir"])
        .arg(&dir)
        .env_remove("GITHUB_ACTIONS")
        .env_remove("LOCALRUNNER_STATE_DIR")
        .output()
        .expect("the binary runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with(&format!(
            "state directory: {} (from --state-dir)\n",
            dir.display()
        )),
        "{err}"
    );
}

#[test]
fn the_state_directory_note_names_where_the_directory_came_from() {
    let layout = lr_cli::paths::Layout::new("/tmp/s".into(), "/tmp/s".into());
    let path = std::path::Path::new("/tmp/s");
    let env = std::ffi::OsStr::new("/tmp/s");
    assert_eq!(
        layout.state_dir_note(Some(path), Some(env)).unwrap(),
        format!(
            "state directory: {} (from --state-dir)",
            layout.root.display()
        )
    );
    assert_eq!(
        layout.state_dir_note(None, Some(env)).unwrap(),
        format!(
            "state directory: {} (from LOCALRUNNER_STATE_DIR)",
            layout.root.display()
        )
    );
    assert_eq!(layout.state_dir_note(None, None), None);
    assert_eq!(
        layout.state_dir_note(None, Some(std::ffi::OsStr::new(""))),
        None
    );
}

#[test]
fn version_prints_binary_and_protocol_version() {
    let out = localrunner(&["--version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
    assert!(
        text.contains(&format!(
            "protocol {}.{}",
            PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor
        )),
        "{text}"
    );
}

fn summary(job: &str, state: JobState) -> JobSummary {
    JobSummary {
        job: JobId(job.into()),
        client_label: "dev".into(),
        state,
        platform: None,
        runner: Some("local:r1".into()),
        queued_at: Timestamp(1),
        started_at: None,
        finished_at: None,
        exit_code: None,
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
        cancelled_by: None,
    }
}

#[test]
fn status_json_has_project_source_label() {
    let view = StatusView {
        running: vec![RunningView {
            summary: summary("j1", JobState::Running),
            steps: Vec::new(),
            last_lines: Vec::new(),
        }],
        queue: vec![QueueItem {
            summary: summary("j2", JobState::Queued),
            position: 1,
            wait_reason: Some("runner busy".into()),
            wait_code: None,
        }],
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 0,
            total_mem_mb: 1024,
            used_cpu: 0,
            total_cpu: 1,
        },
        next: None,
    };
    let json = status_json(&[("local".to_string(), view)]);
    let brokers = json["brokers"].as_array().expect("brokers array");
    assert_eq!(brokers[0]["broker"], "local");
    for (list, job) in [("running", "j1"), ("queue", "j2")] {
        let item = &brokers[0][list][0];
        assert_eq!(item["job"], job, "{list}: {item}");
        assert_eq!(item["project"], "owner/repo", "{list}: {item}");
        assert_eq!(item["source"], "session", "{list}: {item}");
        assert_eq!(item["label"], "agent-1", "{list}: {item}");
    }
    assert_eq!(brokers[0]["queue"][0]["wait_reason"], "runner busy");
}

#[test]
fn status_shows_note_branch_directory_and_age_of_each_job() {
    let mut queued = summary("j2", JobState::Queued);
    queued.queued_at = Timestamp(10_000);
    queued.note = Some("fix flaky test".into());
    queued.branch = Some("feat/x".into());
    queued.cwd = Some("/work/repo".into());
    queued.labels = vec!["release".into(), "work".into()];
    let view = StatusView {
        running: Vec::new(),
        queue: vec![QueueItem {
            summary: queued,
            position: 1,
            wait_reason: None,
            wait_code: None,
        }],
        recent: Vec::new(),
        pool: PoolLoad {
            used_mem_mb: 0,
            total_mem_mb: 1024,
            used_cpu: 0,
            total_cpu: 1,
        },
        next: None,
    };
    let views = [("local".to_string(), view)];
    let now = Timestamp(195_000);
    let text = status_text(&views, now);
    assert!(
        text.contains("  j2  Queued  owner/repo  session  agent-1  local:r1  3m05s  #1"),
        "{text}"
    );
    assert!(
        text.contains("branch feat/x  dir /work/repo  note: fix flaky test  labels: release, work"),
        "{text}"
    );
    let mut json = status_json(&views);
    add_ages(&mut json, now);
    let item = &json["brokers"][0]["queue"][0];
    assert_eq!(item["age_s"], 185);
    assert_eq!(item["note"], "fix flaky test");
    assert_eq!(item["branch"], "feat/x");
    assert_eq!(item["cwd"], "/work/repo");
    assert_eq!(item["labels"], serde_json::json!(["release", "work"]));
    let bare = serde_json::to_value(summary("j3", JobState::Queued)).unwrap();
    assert!(
        bare.get("note").is_none() && bare.get("cwd").is_none() && bare.get("labels").is_none(),
        "{bare}"
    );
    let old: JobSummary = serde_json::from_value({
        let mut v = bare.clone();
        v.as_object_mut().unwrap().remove("source");
        v
    })
    .unwrap();
    assert_eq!(old.note, None);
}

#[derive(Default)]
struct Recorder(Mutex<Vec<AdminRequest>>);

#[async_trait]
impl AdminApi for Recorder {
    async fn handle(&self, _peer: &Peer, req: AdminRequest) -> AdminReply {
        self.0.lock().unwrap().push(req);
        AdminReply::Ok {
            warnings: Vec::new(),
        }
    }
}

async fn admin_call(net: &FakeNetwork, peer: Peer) -> Option<AdminReply> {
    let t = net.dialer(peer).dial("admin", None).await.expect("dial");
    let hello = IpcOpen::Hello {
        version: PROTOCOL_VERSION,
        channel: IpcChannel::Admin,
    };
    t.send(Frame::json(&hello).unwrap()).await.unwrap();
    let opened: IpcOpenReply = tokio::time::timeout(Duration::from_secs(5), t.recv())
        .await
        .expect("answer in time")
        .ok()?
        .parse()
        .unwrap();
    if !matches!(opened, IpcOpenReply::Ready { .. }) {
        return None;
    }
    t.send(Frame::json(&AdminRequest::PolicyReload).unwrap())
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), t.recv())
        .await
        .expect("answer in time")
        .ok()?;
    Some(reply.parse().unwrap())
}

#[tokio::test]
async fn admin_command_over_network_not_accepted() {
    let clock = Arc::new(ManualClock::new(Timestamp(1)));
    let net = FakeNetwork::new(clock);
    let server_peer = Peer {
        endpoint: Endpoint::Ipc {
            os_user: "owner".into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let listener = Arc::new(net.listen("admin", server_peer));
    let api = Arc::new(Recorder::default());
    tokio::spawn(serve(listener, api.clone()));

    let remote = Peer {
        endpoint: Endpoint::Addr("10.0.0.9".parse().unwrap()),
        key: Some("pk".into()),
        access: PeerAccess::Member,
    };
    assert_eq!(admin_call(&net, remote).await, None);
    assert!(
        api.0.lock().unwrap().is_empty(),
        "a network peer reached the admin API"
    );

    let local = Peer {
        endpoint: Endpoint::Ipc {
            os_user: "owner".into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    assert_eq!(
        admin_call(&net, local).await,
        Some(AdminReply::Ok {
            warnings: Vec::new()
        })
    );
    assert_eq!(*api.0.lock().unwrap(), vec![AdminRequest::PolicyReload]);
}

#[test]
fn mem_flag_takes_megabytes_or_a_unit_suffix() {
    use lr_cli::commands::parse_mem_mb;
    for (text, mb) in [
        ("6144", 6144),
        ("6g", 6144),
        ("6G", 6144),
        ("512m", 512),
        ("512M", 512),
        ("1g", 1024),
    ] {
        assert_eq!(parse_mem_mb(text), Ok(mb), "{text}");
    }
    for bad in [
        "",
        "0",
        "0g",
        "g",
        "6x",
        "-1",
        "1.5g",
        "6 g",
        "99999999999999999999g",
    ] {
        let err = parse_mem_mb(bad).expect_err(bad);
        assert!(err.contains("6g") && err.contains("512m"), "{bad}: {err}");
    }
}

#[test]
fn submit_and_run_accept_mem_and_cpu() {
    use clap::Parser;
    use lr_cli::commands::{Cli, Command};
    for cmd in ["submit", "run"] {
        let cli = Cli::try_parse_from([
            "localrunner",
            cmd,
            "unit",
            "--as",
            "a",
            "--mem",
            "6g",
            "--cpu",
            "4",
        ])
        .unwrap();
        let order = match cli.command {
            Command::Submit { order, .. } | Command::Run { order } => order,
            other => panic!("{other:?}"),
        };
        assert_eq!((order.mem, order.cpu), (Some(6144), Some(4)));
        let plain = Cli::try_parse_from(["localrunner", cmd, "unit", "--as", "a"]).unwrap();
        let order = match plain.command {
            Command::Submit { order, .. } | Command::Run { order } => order,
            other => panic!("{other:?}"),
        };
        assert_eq!((order.mem, order.cpu), (None, None));
    }
    for args in [["--mem", "6x"], ["--cpu", "0"], ["--cpu", "two"]] {
        let err =
            Cli::try_parse_from(["localrunner", "run", "unit", "--as", "a", args[0], args[1]])
                .unwrap_err()
                .to_string();
        assert!(err.contains(args[0]), "{err}");
    }
}

#[test]
fn submit_warnings_go_to_the_error_stream_one_per_line() {
    let mut err = Vec::new();
    let warnings = vec![
        "warning: uncommitted changes are ignored: the runner checks out the pushed commit abc1234"
            .to_string(),
    ];
    lr_cli::commands::jobs::print_warnings(&warnings, &mut err);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        format!("{}\n", warnings[0])
    );
}

fn write_handle(dir: &std::path::Path, job: &str, label: &str, at: u64) {
    let runner = dir.join("temp").join("runner");
    std::fs::create_dir_all(&runner).unwrap();
    let body = serde_json::json!({
        "job": job, "label": label, "profile": "unit", "submitted_at": at, "detached": false
    });
    std::fs::write(runner.join(format!("{job}.json")), body.to_string()).unwrap();
}

#[test]
fn jobs_label_comes_from_the_last_handle_in_the_directory() {
    use lr_cli::commands::jobs::label_from_last_handle;
    let d = common::TempDir::new("jobs");
    write_handle(d.path(), "j-1", "agent-a", 100);
    write_handle(d.path(), "j-3", "agent-c", 300);
    write_handle(d.path(), "j-2", "agent-b", 200);
    std::fs::write(d.path().join("temp/runner/j-3.log"), "log").unwrap();
    std::fs::write(d.path().join("temp/runner/queue.json"), "{not a handle").unwrap();
    assert_eq!(label_from_last_handle(d.path()).unwrap(), "agent-c");
}

#[test]
fn jobs_without_as_and_without_a_handle_names_the_flag() {
    use lr_cli::commands::jobs::label_from_last_handle;
    let d = common::TempDir::new("jobs");
    let e = label_from_last_handle(d.path()).unwrap_err();
    assert!(e.contains("--as") && e.contains("temp"), "{e}");
}

#[test]
fn jobs_mine_without_as_is_not_a_usage_error() {
    let d = common::TempDir::new("jobs");
    let out = Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["jobs", "--mine"])
        .current_dir(d.path())
        .env(
            "LOCALRUNNER_STATE_DIR",
            std::env::temp_dir().join("lr-cli-no-state"),
        )
        .output()
        .expect("the binary runs");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no handle"), "{err}");
}

#[test]
fn jobs_label_ignores_oversized_and_non_regular_handles() {
    use lr_cli::commands::jobs::label_from_last_handle;
    let d = common::TempDir::new("jobs");
    write_handle(d.path(), "j-1", "agent-a", 100);
    let big = serde_json::json!({
        "job": "j-9", "label": "huge", "profile": "unit", "submitted_at": 900,
        "detached": false, "pad": "x".repeat(70 * 1024)
    });
    std::fs::write(d.path().join("temp/runner/j-9.json"), big.to_string()).unwrap();
    std::fs::create_dir(d.path().join("temp/runner/j-8.json")).unwrap();
    #[cfg(unix)]
    {
        let target = d.path().join("elsewhere.json");
        let body = serde_json::json!({"label": "linked", "submitted_at": 800});
        std::fs::write(&target, body.to_string()).unwrap();
        std::os::unix::fs::symlink(&target, d.path().join("temp/runner/j-7.json")).unwrap();
    }
    assert_eq!(label_from_last_handle(d.path()).unwrap(), "agent-a");
}

#[test]
fn down_without_flags_drains() {
    use clap::Parser;
    use lr_cli::commands::down_cmd::down_mode;
    use lr_cli::commands::{Cli, Command};
    use lr_proto::DownMode;
    let mode = |args: &[&str]| {
        let mut argv = vec!["localrunner", "down"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Down { cancel, .. } => down_mode(cancel),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(mode(&[]), DownMode::Drain);
    assert_eq!(mode(&["r1"]), DownMode::Drain);
    assert_eq!(mode(&["--drain"]), DownMode::Drain);
    assert_eq!(mode(&["--cancel"]), DownMode::Cancel);
    assert!(Cli::try_parse_from(["localrunner", "down", "--drain", "--cancel"]).is_err());
}

#[test]
fn jobs_mine_prints_project_source_and_label_together() {
    use lr_cli::commands::jobs::mine_line;
    assert_eq!(
        mine_line(&summary("j1", JobState::Running)),
        "j1\tRunning\tunit\towner/repo\tsession\tagent-1"
    );
    let mut bare = summary("j2", JobState::Queued);
    (bare.project, bare.source, bare.label) = (None, None, None);
    assert_eq!(mine_line(&bare), "j2\tQueued\tunit\t-\t-\t-");
}

#[test]
fn wait_takes_several_jobs_or_a_label_and_log_takes_a_label() {
    use clap::Parser;
    use lr_cli::commands::Cli;
    for args in [
        &["wait", "j1", "j2"][..],
        &["wait", "--as", "agent-1"][..],
        &["wait", "j1", "--as", "agent-1"][..],
        &["log", "j1", "--as", "agent-1"][..],
    ] {
        let mut argv = vec!["localrunner"];
        argv.extend(args);
        assert!(Cli::try_parse_from(&argv).is_ok(), "{args:?}");
    }
    assert!(Cli::try_parse_from(["localrunner", "wait"]).is_err());
}

#[test]
fn wait_follows_the_given_ids_the_active_jobs_of_a_label_or_its_last_job() {
    use lr_cli::commands::jobs::wait_targets;
    let mut running = summary("j2", JobState::Running);
    running.profile = Some("gate".into());
    let done = summary("j1", JobState::Passed);
    let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let job = |s: &str| JobId(s.into());
    assert_eq!(
        wait_targets(&ids(&["j9", "j8"]), None, &[], &[]).unwrap(),
        vec![(job("j9"), None), (job("j8"), None)]
    );
    assert_eq!(
        wait_targets(
            &[],
            Some("a"),
            &[running.clone()],
            &[done.clone(), running.clone()]
        )
        .unwrap(),
        vec![(job("j2"), Some("gate".into()))]
    );
    assert_eq!(
        wait_targets(&[], Some("a"), &[], std::slice::from_ref(&done)).unwrap(),
        vec![(job("j1"), Some("unit".into()))]
    );
    assert_eq!(
        wait_targets(&[], Some("a"), &[], &[]).unwrap_err(),
        "no jobs of label a"
    );
    assert_eq!(
        wait_targets(&ids(&["j1"]), Some("a"), &[], &[done]).unwrap(),
        vec![(job("j1"), Some("unit".into()))]
    );
    assert_eq!(
        wait_targets(&ids(&["j7"]), Some("a"), &[], &[]).unwrap_err(),
        "job j7 is not a job of label a known to the hub"
    );
}

#[test]
fn a_wait_line_names_the_job_the_profile_and_the_state() {
    use lr_cli::commands::jobs::{wait_line, Ended};
    let ended = |state| Ended::Exit {
        state,
        exit_code: Some(0),
        reason: None,
    };
    let job = JobId("j1".into());
    assert_eq!(
        wait_line(&job, Some("unit"), &ended(JobState::Passed)),
        "j1 unit Passed"
    );
    assert_eq!(
        wait_line(&job, None, &ended(JobState::Failed)),
        "j1 - Failed"
    );
    assert_eq!(wait_line(&job, None, &Ended::Rejected), "j1 - Rejected");
}

#[test]
fn a_failed_wait_is_one_line_with_code_1_and_does_not_end_the_others() {
    use lr_cli::commands::jobs::{wait_result, Ended};
    let job = JobId("j1".into());
    let (line, code) = wait_result(&job, Some("unit"), Err("not found".into()), false);
    assert_eq!((line.as_str(), code), ("j1 unit Error: not found", 1));
    let (json, code) = wait_result(&job, None, Err("not found".into()), true);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        (v["job"].as_str(), v["error"].as_str(), code),
        (Some("j1"), Some("not found"), 1)
    );
    let ok = Ended::Exit {
        state: JobState::Passed,
        exit_code: Some(0),
        reason: None,
    };
    assert_eq!(
        wait_result(&job, Some("unit"), Ok(ok), false),
        ("j1 unit Passed".to_string(), 0)
    );
}

#[test]
fn the_end_notice_of_a_followed_job_is_silent_inside_a_wait_over_several_jobs() {
    use lr_cli::commands::jobs::exit_notice;
    let job = JobId("j1".into());
    assert_eq!(
        exit_notice(&job, JobState::Passed, None, true).as_deref(),
        Some("job j1: Passed")
    );
    assert_eq!(exit_notice(&job, JobState::Passed, None, false), None);
}

#[cfg(unix)]
#[test]
fn prepare_ipc_failure_names_the_directory() {
    let dir = common::TempDir::new("ipc-dir");
    let root = dir.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("ipc"), b"").unwrap();
    let layout = lr_cli::paths::Layout::new(root.clone(), dir.path().join("config"));
    let e = layout.prepare_ipc().unwrap_err().to_string();
    assert!(e.contains(&root.join("ipc").display().to_string()), "{e}");
}

fn detail_with_label(label: Option<&str>) -> JobDetail {
    let mut s = summary("j1", JobState::Running);
    s.label = label.map(String::from);
    JobDetail {
        summary: s,
        note: None,
        branch: None,
        cwd: None,
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
fn cancel_parses_without_as_so_the_message_can_name_the_label() {
    use clap::Parser;
    let cli = lr_cli::commands::Cli::try_parse_from(["localrunner", "cancel", "j1"]).unwrap();
    assert!(matches!(
        cli.command,
        lr_cli::commands::Command::Cancel { label: None, .. }
    ));
}

#[test]
fn cancel_without_as_names_the_label_of_a_known_job() {
    let d = detail_with_label(Some("agent-1"));
    assert_eq!(
        missing_label_text(Some("j1"), LabelLookup::Known(Some(&d))),
        "cancel: --as <LABEL> is required; the label of j1 is agent-1 (use --force for a job of another label)"
    );
}

#[test]
fn cancel_without_as_says_unknown_for_an_unknown_job() {
    let text = missing_label_text(Some("j9"), LabelLookup::Unknown);
    assert!(text.starts_with("cancel: --as <LABEL> is required; job j9 is not known"));
    assert!(text.contains("label is unknown"), "{text}");
}

#[test]
fn cancel_without_as_does_not_name_a_label_the_status_hides() {
    let d = detail_with_label(None);
    let text = missing_label_text(Some("j1"), LabelLookup::Known(Some(&d)));
    assert!(text.contains("not shown to you"), "{text}");
    assert!(!text.contains("agent-1"));
    assert_eq!(
        missing_label_text(Some("j1"), LabelLookup::Unasked),
        "cancel: --as <LABEL> is required"
    );
    assert_eq!(
        missing_label_text(None, LabelLookup::Unasked),
        "cancel: --as <LABEL> is required"
    );
}
