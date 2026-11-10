mod common;

use common::TempDir;
use lr_cli::commands::down_cmd::down;
use lr_cli::commands::history_cmd::fetch_history;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::log_cmd::{find_log, LogQuery};
use lr_cli::commands::read_cmd::{explain, fetch_runners};
use lr_cli::commands::up_cmd::{running_roles, up_plan};
use lr_cli::commands::view::known_brokers;
use lr_cli::init::NodeRole;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::{JobId, JobState};
use lr_proto::{CliReply, CliRequest, HistoryFilter, SubmitRequest};
use std::time::Duration;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    std::fs::write(
        config.join("policy.toml"),
        format!("[[local_user]]\nname = {user:?}\n"),
    )
    .unwrap();
    let runner = root.join("runner").join("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n",
    )
    .unwrap();
    Layout::new(root, config)
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..150 {
        if let Some(v) = probe().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn run_job(layout: &Layout, dir: &TempDir) -> JobId {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    std::fs::write(
        repo.join(".localrunner").join("profiles.toml"),
        "[profile.unit]\ncommand = \"echo read-me api_token=hunter2hunter2\"\ntimeout_s = 60\nmem_mb = 256\n",
    )
    .unwrap();
    common::init_repo(&repo);
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-q", "-m", "profiles"]);
    let identity = common::session("reader", None, &repo).expect("identity");
    let client = hub(layout).await.expect("hub channel");
    let submit = CliRequest::Submit(SubmitRequest {
        identity,
        profile: Some("unit".into()),
        platform: None,
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        detach: false,
        wait: true,
    });
    let job = match client.call::<_, CliReply>(&submit).await.expect("submit") {
        CliReply::Submitted { job } => job,
        other => panic!("submit answered {other:?}"),
    };
    let state = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match client.recv::<CliReply>().await.expect("hub answer") {
                CliReply::Exit { job: j, state, .. } if j == job => return state,
                CliReply::Rejected { code, detail } => panic!("rejected {code}: {detail}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the job finishes");
    client.close().await;
    assert_eq!(state, JobState::Passed);
    job
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_commands_and_full_down_on_loopback() {
    let dir = TempDir::new("reads");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    eventually("runner r1 online", || async {
        let client = hub(&layout).await.ok()?;
        let brokers = known_brokers(&client).await.ok()?;
        brokers.iter().find(|b| b.available)?;
        let (per, _) = fetch_runners(&client, None).await.ok()?;
        client.close().await;
        per.iter()
            .flat_map(|(_, r)| r)
            .any(|r| r.online && r.name.ends_with("r1"))
            .then_some(())
    })
    .await;
    let job = run_job(&layout, &dir).await;

    let client = hub(&layout).await.expect("hub channel");
    let mine = HistoryFilter {
        mine: true,
        ..HistoryFilter::default()
    };
    let (per, problems) = fetch_history(&client, None, &mine).await.expect("history");
    assert!(problems.is_empty(), "{problems:?}");
    assert!(
        per.iter()
            .any(|(b, jobs)| b == "local" && jobs.iter().any(|s| s.job == job)),
        "{per:?}"
    );
    let (broker_name, text) = explain(&client, &job, None).await.expect("explain");
    assert_eq!(broker_name, "local");
    assert!(text.contains("passed"), "{text}");
    let mut log = String::new();
    let query = LogQuery {
        job: job.clone(),
        step: None,
        tail: None,
        follow: true,
    };
    find_log(&client, None, &query, &mut |t| log.push_str(t))
        .await
        .expect("log of a finished job ends");
    assert!(log.contains("read-me"), "{log}");
    assert!(
        !log.contains("hunter2hunter2"),
        "the CLI masks secrets: {log}"
    );
    let missing = LogQuery {
        job: JobId("no-such-job".into()),
        ..query
    };
    let err = find_log(&client, None, &missing, &mut |_| {})
        .await
        .expect_err("unknown job");
    assert!(err.contains("no-such-job"), "{err}");
    client.close().await;

    let all = [NodeRole::Hub, NodeRole::Broker, NodeRole::Runner];
    let up = running_roles(&layout).await;
    assert!(up.hub && up.broker && up.runners.contains("r1"), "{up:?}");
    assert!(
        up_plan(&layout, &all, &up).is_empty(),
        "nothing is started twice"
    );

    let stopping = {
        let layout = layout.clone();
        tokio::spawn(async move { down(&layout, Vec::new(), false).await })
    };
    let within = Duration::from_secs(20);
    tokio::time::timeout(within, runners.wait())
        .await
        .expect("the runner stops");
    tokio::time::timeout(within, hub_role.wait())
        .await
        .expect("the hub stops");
    tokio::time::timeout(within, broker.wait())
        .await
        .expect("the broker stops");
    let code = tokio::time::timeout(within, stopping)
        .await
        .expect("down ends")
        .expect("down task");
    assert_eq!(code, Ok(0));

    let gone = running_roles(&layout).await;
    assert!(
        !gone.hub && !gone.broker && gone.runners.is_empty(),
        "{gone:?}"
    );
    let plan = up_plan(&layout, &all, &gone);
    let words: Vec<String> = plan.iter().map(|argv| argv.join(" ")).collect();
    assert_eq!(words.len(), 3, "{words:?}");
    assert!(words[0].starts_with("broker run --background"), "{words:?}");
    assert!(words[1].starts_with("hub run --background"), "{words:?}");
    assert!(
        words[2].starts_with("runner run --name r1 --background"),
        "{words:?}"
    );
    assert!(words.iter().all(|w| w.contains("--state-dir")), "{words:?}");
}
