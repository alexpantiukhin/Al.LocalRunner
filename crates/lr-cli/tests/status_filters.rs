mod common;

use common::TempDir;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::view::known_brokers;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::{GithubInfo, JobId, Source};
use lr_proto::{CliReply, CliRequest, SubmitRequest};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
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
        format!(
            "allow_elevated = true\nmem_mb = 1024\ncpu = 1\n{}",
            common::env_path_toml()
        ),
    )
    .unwrap();
    Layout::new(root, config)
}

fn repo(dir: &TempDir) -> std::path::PathBuf {
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    let slow = if cfg!(windows) {
        "ping -n 120 127.0.0.1"
    } else {
        "sleep 120"
    };
    std::fs::write(
        repo.join(".localrunner").join("profiles.toml"),
        format!("[profile.slow]\ncommand = \"{slow}\"\ntimeout_s = 300\nmem_mb = 256\n"),
    )
    .unwrap();
    common::init_repo(&repo);
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-q", "-m", "profiles"]);
    repo
}

async fn place(layout: &Layout, identity: lr_core::Identity) -> JobId {
    let client = hub(layout).await.expect("hub channel");
    let req = CliRequest::Submit(SubmitRequest {
        identity,
        profile: Some("slow".into()),
        platform: None,
        timeout_s: None,
        mem_mb: None,
        cpu: None,
        args: Vec::new(),
        detach: true,
        wait: false,
    });
    let job = match client.call::<_, CliReply>(&req).await.expect("submit") {
        CliReply::Submitted { job, .. } => job,
        other => panic!("submit answered {other:?}"),
    };
    client
        .send(&CliRequest::Done { job: job.clone() })
        .await
        .unwrap();
    client.close().await;
    job
}

fn cli(dir: &TempDir, cwd: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(args)
        .current_dir(cwd)
        .env("LOCALRUNNER_STATE_DIR", dir.path().join("state"))
        .env("LOCALRUNNER_CONFIG_DIR", dir.path().join("config"))
        .output()
        .expect("the binary runs");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn status_jobs(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v["brokers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| {
            let running = b["running"].as_array().unwrap().iter();
            let queue = b["queue"].as_array().unwrap().iter();
            running
                .chain(queue)
                .map(|j| j["job"].as_str().unwrap().to_string())
        })
        .collect();
    out.sort();
    out
}

fn sorted(jobs: &[&JobId]) -> Vec<String> {
    let mut v: Vec<String> = jobs.iter().map(|j| j.0.clone()).collect();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_jobs_and_brokers_filter_and_print_json_through_the_hub() {
    let dir = TempDir::new("status-filters");
    let layout = layout(&dir);
    let repo = repo(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    for _ in 0..150 {
        let client = hub(&layout).await.unwrap();
        let online = match fetch_runners(&client, None).await {
            Ok((per, _)) => per
                .iter()
                .flat_map(|(_, r)| r)
                .any(|r| r.online && r.name.ends_with("r1")),
            Err(_) => false,
        };
        let ready = online
            && known_brokers(&client)
                .await
                .is_ok_and(|b| b.iter().any(|b| b.available));
        client.close().await;
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let agent = common::session("agent", Some("first"), &repo).unwrap();
    let mut other = agent.clone();
    other.project = "acme/other".into();
    let mut ci = agent.clone();
    ci.label = "ci".into();
    ci.source = Source::Github(GithubInfo {
        repository: agent.project.clone(),
        workflow: "checks".into(),
        job: "unit".into(),
        run_id: 7,
        attempt: Some(1),
        git_ref: "refs/heads/main".into(),
        actor: "alex".into(),
        run_url: "https://github.com/acme/app/actions/runs/7".into(),
    });
    let a = place(&layout, agent.clone()).await;
    let o = place(&layout, other).await;
    let c = place(&layout, ci).await;

    let all = cli(&dir, &repo, &["status", "--json"]);
    assert_eq!(status_jobs(&all), sorted(&[&a, &o, &c]));
    let project = cli(
        &dir,
        &repo,
        &["status", "--json", "--project", "acme/other"],
    );
    assert_eq!(status_jobs(&project), sorted(&[&o]));
    let github = cli(&dir, &repo, &["status", "--json", "--source", "github"]);
    assert_eq!(status_jobs(&github), sorted(&[&c]));
    let session = cli(&dir, &repo, &["status", "--json", "--source", "session"]);
    assert_eq!(status_jobs(&session), sorted(&[&a, &o]));
    let label = cli(&dir, &repo, &["status", "--json", "--as", "ci"]);
    assert_eq!(status_jobs(&label), sorted(&[&c]));
    let named = cli(
        &dir,
        &repo,
        &["status", "--json", "--broker", "local", "--as", "agent"],
    );
    assert_eq!(status_jobs(&named), sorted(&[&a, &o]));
    let elsewhere = cli(&dir, &repo, &["status", "--json", "--broker", "nowhere"]);
    assert!(status_jobs(&elsewhere).is_empty());
    let first = ["running", "queue"]
        .iter()
        .flat_map(|l| all["brokers"][0][l].as_array().unwrap().clone())
        .find(|j| j["job"] == a.0.as_str())
        .unwrap();
    assert_eq!(first["project"], agent.project.as_str());
    assert_eq!(first["label"], "agent");
    assert_eq!(first["profile"], "slow");
    assert_eq!(first["note"], "first");
    assert_eq!(first["cwd"], agent.cwd.display().to_string().as_str());
    assert_eq!(first["branch"], agent.branch.as_deref().unwrap());
    assert!(first["age_s"].as_u64().is_some_and(|a| a < 600), "{first}");

    let jobs = cli(
        &dir,
        &repo,
        &["jobs", "--active", "--json", "--as", "agent"],
    );
    let listed: Vec<String> = {
        let mut v: Vec<String> = jobs
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["job"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(listed, sorted(&[&a, &o]));
    for j in jobs.as_array().unwrap() {
        let state = j["state"].as_str().unwrap();
        assert!(state == "queued" || state == "running", "{j}");
    }

    let brokers = cli(&dir, &repo, &["brokers", "--json"]);
    let list = brokers.as_array().unwrap();
    assert_eq!(list.len(), 1, "{brokers}");
    assert_eq!(list[0]["name"], "local");
    assert_eq!(list[0]["available"], true);
    assert_eq!(list[0]["zone"], "local");
    assert_eq!(list[0]["outbox"], 0);

    runners.stop();
    hub_role.stop();
    broker.stop();
}
