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
