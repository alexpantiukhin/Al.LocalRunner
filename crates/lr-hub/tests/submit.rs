mod common;

use common::broker::{OnOrder, Script};
use common::*;
use lr_abstractions::OutboxKind;
use lr_core::{JobState, Zone};
use lr_proto::{
    Accepted, BrokerToHub, CliReply, CliRequest, ProtocolErrorCode, ReasonCode, RejectReason,
    WireCode,
};

#[tokio::test]
async fn submit_returns_jobid_and_keeps_order_in_outbox_until_accepted() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner().on_order(OnOrder::Silent));
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    assert_eq!(
        rig.pending().await,
        vec![(job.clone(), 1, OutboxKind::Order)]
    );
    eventually("order reaches the broker", || local.orders().len() == 1).await;
    let order = &local.orders()[0];
    assert_eq!((order.job.clone(), order.seq), (job.clone(), 1));
    settle().await;
    assert_eq!(rig.pending().await.len(), 1, "no Accepted yet");
    local
        .send(BrokerToHub::Accepted(Accepted {
            job: job.clone(),
            order_seq: 1,
            waiting: None,
        }))
        .await;
    settle().await;
    assert!(rig.pending().await.is_empty());
    let active = rig.active(&job).await.expect("still active after Accepted");
    assert_eq!(active.broker.as_deref(), Some("local"));
}

#[tokio::test]
async fn work_tree_order_carries_cwd_profile_and_identity() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    eventually("order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.profile, "unit");
    assert_eq!(
        spec.code,
        WireCode::WorkTree {
            cwd: repo_root().display().to_string()
        }
    );
    assert_eq!(spec.requirements.platform, None);
    assert!(spec.requirements.same_machine.is_some());
    assert_eq!(spec.requirements.timeout_s, 600);
    assert_eq!(spec.requirements.mem_mb, 1024);
    assert_eq!(spec.steps[0].command, "make test");
    assert_eq!(spec.identity.label, "agent");
    assert_eq!(spec.enqueued_at.0, START);
    let a = rig.active(&job).await.unwrap();
    assert_eq!(
        (a.submitter_pid, a.submitter_start),
        (SENDER_PID, Some(SENDER_START))
    );
    assert_eq!((a.label.as_str(), a.os_user.as_str()), ("agent", "alex"));
}

#[tokio::test]
async fn platform_order_reads_profile_from_commit_with_repo_url_and_branch() {
    let mut rig = Rig::new();
    rig.files.put(
        &repo_root().join(".localrunner").join("profiles.toml"),
        PROFILES
            .replace("make check", "edited in work tree")
            .as_bytes(),
    );
    rig.join("lan1").await;
    let lan = rig.broker("lan1", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    eventually("connected", || lan.resumed()).await;
    rig.cli()
        .await
        .submit(submit("remote", Some("linux")))
        .await;
    eventually("order sent", || lan.orders().len() == 1).await;
    let spec = &lan.orders()[0].spec;
    assert_eq!(
        spec.code,
        WireCode::Commit {
            sha: COMMIT.into(),
            repo_url: Some(ORIGIN.into()),
            branch: Some("main".into())
        }
    );
    assert_eq!(
        spec.steps[0].command, "make check",
        "profile read from the commit"
    );
    assert_eq!(spec.requirements.platform.as_deref(), Some("linux"));
    assert_eq!(spec.requirements.same_machine, None);
}

#[tokio::test]
async fn single_platform_of_profile_is_used_without_platform_flag() {
    let mut rig = Rig::new();
    rig.join("lan1").await;
    let lan = rig.broker("lan1", Zone::Lan, Script::linux(Zone::Lan));
    rig.start();
    eventually("connected", || lan.resumed()).await;
    rig.cli().await.submit(submit("remote", None)).await;
    eventually("order sent", || lan.orders().len() == 1).await;
    assert_eq!(
        lan.orders()[0].spec.requirements.platform.as_deref(),
        Some("linux")
    );
}

#[tokio::test]
async fn unpushed_commit_is_refused_without_jobid() {
    let mut rig = Rig::new();
    rig.repo.set_head(&repo_root(), "fff0000", Some(ORIGIN));
    rig.start();
    let cli = rig.cli().await;
    cli.send(CliRequest::Submit(submit("remote", Some("linux"))))
        .await;
    match cli.reply().await {
        CliReply::Rejected { code, detail } => {
            assert_eq!(code, ReasonCode::Rejected(RejectReason::CommitNotPushed));
            assert!(detail.contains("fff0000"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn timeout_flag_only_lowers_the_profile_timeout() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mut up = submit("unit", None);
    up.timeout_s = Some(601);
    cli.send(CliRequest::Submit(up)).await;
    assert!(
        matches!(cli.reply().await, CliReply::Error(e) if e.code == ProtocolErrorCode::Malformed)
    );
    let mut down = submit("unit", None);
    down.timeout_s = Some(30);
    cli.submit(down).await;
    eventually("order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.requirements.timeout_s, 30);
    assert_eq!(spec.steps[0].timeout_s, 30);
}

#[tokio::test]
async fn unknown_profile_and_missing_file_are_errors() {
    let mut rig = Rig::new();
    rig.start();
    let cli = rig.cli().await;
    cli.send(CliRequest::Submit(submit("nope", None))).await;
    match cli.reply().await {
        CliReply::Error(e) => assert!(e.message.contains("nope"), "{}", e.message),
        other => panic!("{other:?}"),
    }
    lr_abstractions::Files::remove_file(
        &rig.files,
        &repo_root().join(".localrunner").join("profiles.toml"),
    )
    .await
    .unwrap();
    cli.send(CliRequest::Submit(submit("unit", None))).await;
    match cli.reply().await {
        CliReply::Error(e) => assert!(e.message.contains("profiles.toml"), "{}", e.message),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn handle_and_log_copy_written_in_submitter_directory() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let job = rig.cli().await.submit(submit("unit", None)).await;
    let handle = rig.files.file(&handle_path(&job)).expect("handle written");
    let v: serde_json::Value = serde_json::from_slice(&handle).unwrap();
    assert_eq!(v["job"], job.0.as_str());
    assert_eq!(v["label"], "agent");
    assert_eq!(v["profile"], "unit");
    assert_eq!(v["submitted_at"], START);
    let a = rig.active(&job).await.unwrap();
    assert_eq!(
        a.handle_path.map(std::path::PathBuf::from),
        Some(handle_path(&job))
    );
    eventually("accepted", || local.orders().len() == 1).await;
    settle().await;
    local.started(&job).await;
    local.chunk(&job, "hello from the job\n").await;
    local.result(&job, JobState::Passed, Some(0)).await;
    settle().await;
    let log = String::from_utf8(rig.store.file(&log_path(&job)).unwrap()).unwrap();
    assert!(log.contains("hello from the job\n"), "{log}");
    assert!(log.contains("passed"), "{log}");
    assert!(
        rig.active(&job).await.is_none(),
        "final state ends the active order"
    );
}

#[tokio::test]
async fn submitter_directory_with_parent_components_is_refused() {
    let mut rig = Rig::new();
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.identity.cwd = repo_root().join("..").join("..").join("etc");
    req.identity.worktree_root = None;
    cli.send(CliRequest::Submit(req)).await;
    assert!(matches!(cli.reply().await, CliReply::Error(_)));
    let mut rel = submit("unit", None);
    rel.identity.cwd = "relative/dir".into();
    rel.identity.worktree_root = None;
    cli.send(CliRequest::Submit(rel)).await;
    assert!(matches!(cli.reply().await, CliReply::Error(_)));
    assert!(rig.pending().await.is_empty());
}

#[tokio::test]
async fn os_user_comes_from_the_ipc_peer_not_the_message() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.identity.os_user = "mallory".into();
    let job = cli.submit(req).await;
    eventually("order sent", || local.orders().len() == 1).await;
    assert_eq!(local.orders()[0].spec.identity.os_user, "alex");
    assert_eq!(rig.active(&job).await.unwrap().os_user, "alex");
}

#[tokio::test]
async fn network_peer_on_cli_channel_is_refused() {
    let mut rig = Rig::new();
    rig.start();
    let dialer = rig.net.dialer(lr_abstractions::Peer {
        endpoint: lr_core::Endpoint::Addr("10.1.1.1".parse().unwrap()),
        key: Some("k".into()),
        access: lr_abstractions::PeerAccess::Member,
    });
    use lr_abstractions::Dialer;
    let t = dialer.dial(HUB_IPC, None).await.unwrap();
    t.send(
        lr_abstractions::Frame::json(&lr_proto::IpcOpen::Hello {
            version: lr_proto::PROTOCOL_VERSION,
            channel: lr_proto::IpcChannel::Cli,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let reply: lr_proto::IpcOpenReply =
        lr_proto::decode(t.recv().await.unwrap().as_bytes()).unwrap();
    assert!(
        matches!(reply, lr_proto::IpcOpenReply::Refused(_)),
        "{reply:?}"
    );
}

#[tokio::test]
async fn done_only_for_the_job_submitted_on_this_connection() {
    let mut rig = Rig::new();
    let _local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    cli.send(CliRequest::Done {
        job: lr_core::JobId("other".into()),
    })
    .await;
    assert!(matches!(cli.reply().await, CliReply::Error(_)));
    let job = cli.submit(submit("unit", None)).await;
    cli.send(CliRequest::Done { job: job.clone() }).await;
    assert!(cli.recv_frame().await.is_none(), "done has no answer");
    let other = rig.cli().await;
    other.send(CliRequest::Done { job }).await;
    assert!(matches!(other.reply().await, CliReply::Error(_)));
}

#[tokio::test]
async fn job_ids_are_unique_and_path_safe() {
    let mut rig = Rig::new();
    let _local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let a = cli.submit(submit("unit", None)).await;
    let b = cli.submit(submit("unit", None)).await;
    assert_ne!(a, b);
    for id in [&a, &b] {
        assert!(
            id.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{id}"
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn rooted_directory_without_drive_is_not_absolute() {
    let mut rig = Rig::new();
    rig.start();
    let cli = rig.cli().await;
    let mut req = submit("unit", None);
    req.identity.cwd = r"\work\repo".into();
    req.identity.worktree_root = None;
    cli.send(CliRequest::Submit(req)).await;
    match cli.reply().await {
        CliReply::Error(e) => assert!(e.message.contains("must be absolute"), "{}", e.message),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn failed_enqueue_leaves_no_handle_behind() {
    let mut rig = Rig::new();
    let _local = rig.local_broker(Script::local_runner());
    rig.start();
    rig.store.fail("enqueue", 1);
    let cli = rig.cli().await;
    cli.send(CliRequest::Submit(submit("unit", None))).await;
    match cli.reply().await {
        CliReply::Error(e) => assert_eq!(e.code, ProtocolErrorCode::Unavailable),
        other => panic!("{other:?}"),
    }
    let first = lr_core::JobId(format!("j{START:x}-0"));
    assert_eq!(rig.files.file(&handle_path(&first)), None);
    let job = cli.submit(submit("unit", None)).await;
    assert!(rig.files.file(&handle_path(&job)).is_some());
}

#[tokio::test]
async fn mem_and_cpu_flags_replace_profile_resources_for_one_run() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    cli.submit(submit("unit", None)).await;
    let mut over = submit("unit", None);
    over.mem_mb = Some(6144);
    over.cpu = Some(4);
    cli.submit(over).await;
    eventually("orders sent", || local.orders().len() == 2).await;
    let plain = &local.orders()[0].spec;
    assert_eq!(
        (plain.requirements.mem_mb, plain.requirements.cpu),
        (1024, 0)
    );
    assert!(plain.overrides.is_empty());
    let spec = &local.orders()[1].spec;
    assert_eq!((spec.requirements.mem_mb, spec.requirements.cpu), (6144, 4));
    assert_eq!(
        (spec.overrides.mem_mb, spec.overrides.cpu),
        (Some(6144), Some(4))
    );
}

#[tokio::test]
async fn zero_mem_or_cpu_flag_is_malformed() {
    let mut rig = Rig::new();
    rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    for (mem, cpu, flag) in [(Some(0), None, "--mem"), (None, Some(0), "--cpu")] {
        let mut req = submit("unit", None);
        req.mem_mb = mem;
        req.cpu = cpu;
        cli.send(CliRequest::Submit(req)).await;
        match cli.reply().await {
            CliReply::Error(e) => {
                assert_eq!(e.code, ProtocolErrorCode::Malformed);
                assert!(e.message.contains(flag), "{}", e.message);
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn mem_flag_may_lower_the_profile_value() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let mut low = submit("unit", None);
    low.mem_mb = Some(512);
    rig.cli().await.submit(low).await;
    eventually("order sent", || local.orders().len() == 1).await;
    let spec = &local.orders()[0].spec;
    assert_eq!(spec.requirements.mem_mb, 512);
    assert_eq!(spec.overrides.mem_mb, Some(512));
    assert_eq!(spec.overrides.cpu, None);
}
