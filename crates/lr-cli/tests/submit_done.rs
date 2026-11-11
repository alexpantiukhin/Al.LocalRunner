mod common;

use common::TempDir;
use lr_abstractions::fakes::{channel_pair, ManualClock};
use lr_abstractions::{Frame, Peer, PeerAccess, Transport};
use lr_cli::commands::jobs::submit_on;
use lr_cli::ipc_client::IpcClient;
use lr_core::{Endpoint, JobId, JobState, Timestamp};
use lr_obs::Redactor;
use lr_proto::{CliReply, CliRequest, IpcChannel, IpcOpenReply, SubmitRequest, PROTOCOL_VERSION};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<CliRequest>>>;

async fn fake_hub(wait_output: bool) -> (IpcClient, Seen, tokio::task::JoinHandle<()>) {
    let peer = Peer {
        endpoint: Endpoint::Ipc {
            os_user: "alex".into(),
        },
        key: None,
        access: PeerAccess::Local,
    };
    let clock = Arc::new(ManualClock::new(Timestamp(0)));
    let (cli, hub) = channel_pair(clock, peer.clone(), peer);
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
        let _hello = hub.recv().await.expect("hello");
        let ready = IpcOpenReply::Ready {
            version: PROTOCOL_VERSION,
        };
        hub.send(Frame::json(&ready).unwrap()).await.unwrap();
        while let Ok(frame) = hub.recv().await {
            let req: CliRequest = lr_proto::decode(frame.as_bytes()).unwrap();
            let submit = matches!(req, CliRequest::Submit(_));
            log.lock().unwrap().push(req);
            if !submit {
                continue;
            }
            let job = JobId("j-1".into());
            let mut replies = vec![CliReply::Submitted {
                job: job.clone(),
                warnings: Vec::new(),
            }];
            if wait_output {
                replies.push(CliReply::Output {
                    job: job.clone(),
                    text: "built\npassword=letmein1\n".into(),
                });
                replies.push(CliReply::Exit {
                    job,
                    state: JobState::Failed,
                    exit_code: Some(3),
                    reason: None,
                });
            }
            for r in replies {
                hub.send(Frame::json(&r).unwrap()).await.unwrap();
            }
        }
    });
    let client = IpcClient::open(Box::new(cli), IpcChannel::Cli)
        .await
        .expect("the fake hub opens the channel");
    (client, seen, task)
}

fn request(wait: bool) -> SubmitRequest {
    let dir = TempDir::new("submit-done");
    SubmitRequest {
        identity: common::session("agent", None, dir.path()).unwrap(),
        profile: Some("unit".into()),
        platform: None,
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        detach: false,
        wait,
    }
}

#[tokio::test]
async fn submit_says_done_on_the_same_channel_right_after_the_job_id() {
    let (client, seen, task) = fake_hub(false).await;
    let mut printed = String::new();
    let code = submit_on(
        client,
        request(false),
        false,
        &Arc::new(Redactor::new(&[])),
        &mut |t| printed.push_str(t),
    )
    .await;
    assert_eq!(code, Ok(0));
    task.await.unwrap();
    let frames = seen.lock().unwrap().clone();
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert!(matches!(&frames[0], CliRequest::Submit(s) if !s.wait));
    assert_eq!(
        frames[1],
        CliRequest::Done {
            job: JobId("j-1".into())
        }
    );
    assert_eq!(printed, "j-1\n");

    let (client, _, _) = fake_hub(false).await;
    let mut printed = String::new();
    submit_on(
        client,
        request(false),
        true,
        &Arc::new(Redactor::new(&[])),
        &mut |t| printed.push_str(t),
    )
    .await
    .unwrap();
    assert_eq!(printed, "{\"job\":\"j-1\"}\n");
}

#[tokio::test]
async fn run_waits_on_the_same_channel_and_never_says_done() {
    let (client, seen, _task) = fake_hub(true).await;
    let mut printed = String::new();
    let code = submit_on(
        client,
        request(true),
        false,
        &Arc::new(Redactor::new(&[])),
        &mut |t| printed.push_str(t),
    )
    .await;
    assert_eq!(code, Ok(3), "the job's own exit code");
    assert_eq!(printed, "built\npassword=***\n");
    let frames = seen.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert!(matches!(&frames[0], CliRequest::Submit(s) if s.wait));
}
