use crate::common::TempDir;
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::join_cmd::{runner_join, RunnerJoin};
use lr_cli::roles::start_runners;
use lr_core::JobState;
use lr_proto::{AdminReply, AdminRequest, ReasonCode};
use lr_runner::AgentExit;
use std::time::Duration;

const ENDS: Duration = Duration::from_secs(90);

// scenario: 16.4
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_runner_job_is_lost_and_its_key_is_refused() {
    let dir = TempDir::new("e2e-rrm");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, roles) = machine(&dir, "main", "", &[], "").await;
    let node = layout(&dir, "node", "");
    std::fs::write(node.config_file("runner.toml"), runner_toml()).unwrap();
    let (address, code) = join_code(&main, None, None, Some(1)).await;
    let join = RunnerJoin {
        address,
        code,
        name: Some("box".into()),
        count: 1,
        config: None,
        platforms: vec![PLATFORM.into()],
        tags: Vec::new(),
    };
    assert_eq!(runner_join(&node, join).await, Ok(0));
    let remote = start_runners(&node, Some("box"), 1, None, false)
        .await
        .expect("the joined runner starts");
    runner_online(&main, "box").await;

    let extra = format!("mem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n");
    let repo = repo(&dir, &held_profile("held", dir.path(), &gate, 120, &extra));
    push_origin(&dir, &repo);
    let (client, job) = submit(&main, &repo, "held", Some(PLATFORM)).await;
    release.watch(started(dir.path(), "held", "child").await);
    let view = status(&main).await.expect("status");
    assert_eq!(view.pool.used_mem_mb, 256, "{view:?}");

    let known = names(&main, AdminRequest::RunnerList).await;
    assert_eq!(known.len(), 1, "{known:?}");
    let removed = admin(
        &main,
        AdminRequest::RunnerRemove {
            name: known[0].clone(),
        },
    )
    .await;
    assert_eq!(
        removed,
        AdminReply::Ok {
            warnings: Vec::new()
        }
    );
    let ended = until_exit(client, &job, ENDS).await;
    assert_eq!(ended.state, JobState::Lost, "{ended:?}");
    assert_eq!(ended.reason, Some(ReasonCode::RunnerRemoved), "{ended:?}");
    let view = status(&main).await.expect("status");
    assert_eq!(view.pool.used_mem_mb, 0, "resources are back: {view:?}");

    let exits = tokio::time::timeout(ENDS, remote.wait())
        .await
        .expect("the removed runner ends");
    assert!(
        matches!(exits.as_slice(), [AgentExit::Refused(_)]),
        "its key is refused on reconnect: {exits:?}"
    );
    assert!(names(&main, AdminRequest::RunnerList).await.is_empty());
    let left = runners(&main).await.expect("runners");
    assert!(left.is_empty(), "{left:?}");

    roles.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.4
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_client_waiting_orders_end_client_removed() {
    let dir = TempDir::new("e2e-crm");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, roles) = machine(
        &dir,
        "main",
        "[pool]\nmem_mb = 512\n",
        &["r1", "r2"],
        &platform_runner_toml(),
    )
    .await;
    let (guest, guest_hub) = joined_hub(&dir, &main, "guest", "guest", None).await;
    let remote = format!("mem_mb = 256\nplatforms = [\"{PLATFORM}\"]\n");
    let profiles = format!(
        "{}\n{}\n{}",
        held_profile("own", dir.path(), &gate, 120, "mem_mb = 256\n"),
        held_profile("theirs", dir.path(), &gate, 120, &remote),
        echo_profile("waiting", "never-runs", 60, &remote),
    );
    let repo = repo(&dir, &profiles);
    push_origin(&dir, &repo);

    let (own_client, own) = submit(&main, &repo, "own", None).await;
    release.watch(started(dir.path(), "own", "child").await);
    let (theirs_client, theirs) = submit(&guest, &repo, "theirs", Some(PLATFORM)).await;
    release.watch(started(dir.path(), "theirs", "child").await);
    let (waiting_client, waiting) = submit(&guest, &repo, "waiting", Some(PLATFORM)).await;
    queued(&main, &waiting).await;

    let clients = names(&main, AdminRequest::ClientList).await;
    assert_eq!(clients, ["guest"], "{clients:?}");
    let removed = admin(
        &main,
        AdminRequest::ClientRemove {
            name: "guest".into(),
        },
    )
    .await;
    assert_eq!(
        removed,
        AdminReply::Ok {
            warnings: Vec::new()
        }
    );
    let cancelled = row(&main, &waiting).await;
    assert_eq!(cancelled.state, JobState::Cancelled, "{cancelled:?}");
    assert_eq!(cancelled.reason, Some(ReasonCode::ClientRemoved));
    assert!(cancelled.started_at.is_none(), "{cancelled:?}");
    eventually(
        "the removed hub to lose the broker",
        Duration::from_secs(60),
        || async {
            let client = lr_cli::commands::jobs::hub(&guest).await.ok()?;
            let brokers = lr_cli::commands::view::known_brokers(&client).await.ok();
            client.close().await;
            brokers?.iter().all(|b| !b.available).then_some(())
        },
    )
    .await;
    assert!(names(&main, AdminRequest::ClientList).await.is_empty());
    let still = status(&main).await.expect("status");
    assert!(
        still.running.iter().any(|r| r.summary.job == theirs),
        "a running job of the removed client lives by [orphans] running: {still:?}"
    );

    open(&gate);
    let own_end = until_exit(own_client, &own, ENDS).await;
    assert_eq!(own_end.state, JobState::Passed, "{own_end:?}");
    let finished = row(&main, &theirs).await;
    assert_eq!(finished.state, JobState::Passed, "{finished:?}");
    theirs_client.close().await;
    waiting_client.close().await;

    guest_hub.stop();
    roles.stop();
    drop(release);
    remove(dir).await;
}

// scenario: 16.22
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn policy_reload_applies_a_rule_and_a_bad_file_keeps_the_old_one() {
    let dir = TempDir::new("e2e-pol");
    let gate = dir.path().join("gate");
    let mut release = Release::new(&[&gate]);
    let (main, roles) = machine(&dir, "main", "", &["r1", "r2"], &runner_toml()).await;
    let profiles = format!(
        "{}\n{}",
        held_profile("first", dir.path(), &gate, 120, "mem_mb = 256\n"),
        echo_profile("next", "held-by-the-rule", 60, "mem_mb = 256\n"),
    );
    let repo = repo(&dir, &profiles);
    let (first_client, first) = submit(&main, &repo, "first", None).await;
    release.watch(started(dir.path(), "first", "child").await);

    let file = main.config_file("policy.toml");
    let old = std::fs::read_to_string(&file).unwrap();
    let rule = "\n[[rule]]\nname = \"one-at-a-time\"\nquota = { max_running = 1 }\n";
    std::fs::write(&file, format!("{old}{rule}")).unwrap();
    let applied = admin(&main, AdminRequest::PolicyReload).await;
    assert!(matches!(applied, AdminReply::Ok { .. }), "{applied:?}");

    let (next_client, next) = submit(&main, &repo, "next", None).await;
    let held = queued(&main, &next).await;
    let why = held.wait_reason.clone().unwrap_or_default();
    assert!(
        why.contains("правило one-at-a-time"),
        "the delay names the rule: {held:?}"
    );
    let view = status(&main).await.expect("status");
    assert!(
        view.running.iter().any(|r| r.summary.job == first),
        "the running job is untouched by the reload: {view:?}"
    );

    std::fs::write(&file, format!("{old}{rule}\n[grace]\nlost_min = 0\n")).unwrap();
    let refused = admin(&main, AdminRequest::PolicyReload).await;
    let AdminReply::Failed { message } = refused else {
        panic!("a bad file is refused: {refused:?}");
    };
    assert!(
        message.contains("lost_min"),
        "the refusal names why: {message}"
    );
    let kept = queued(&main, &next).await;
    assert_eq!(
        kept.wait_reason, held.wait_reason,
        "the old policy stays: {kept:?}"
    );

    open(&gate);
    let a = until_exit(first_client, &first, ENDS).await;
    assert_eq!(a.state, JobState::Passed, "{a:?}");
    let b = until_exit(next_client, &next, ENDS).await;
    assert_eq!(b.state, JobState::Passed, "{b:?}");
    assert!(b.output.contains("held-by-the-rule"), "{}", b.output);

    roles.stop();
    drop(release);
    remove(dir).await;
}
