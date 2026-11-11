mod common;

use async_trait::async_trait;
use lr_abstractions::fakes::{FakeNetwork, ManualClock};
use lr_abstractions::{Dialer, Frame, Peer, PeerAccess};
use lr_cli::admin::{serve, AdminApi};
use lr_cli::commands::view::status_json;
use lr_core::{Endpoint, JobId, JobState, Timestamp};
use lr_proto::{
    AdminReply, AdminRequest, IpcChannel, IpcOpen, IpcOpenReply, JobSummary, PoolLoad, QueueItem,
    RunningView, StatusView, WireSource, PROTOCOL_VERSION,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn localrunner(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(args)
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
