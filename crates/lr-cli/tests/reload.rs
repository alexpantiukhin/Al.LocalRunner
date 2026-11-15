mod common;

use common::loopback::{layout, until_ready};
use common::TempDir;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::reload_cmd::{reload, reload_all, Outcome};
use lr_cli::commands::RoleArg;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_cli::runner_config::RunnerConfig;
use std::time::Duration;

fn write_runner_toml(layout: &Layout, extra: &str) {
    std::fs::write(
        layout.runner_dir("r1").join("runner.toml"),
        format!(
            "{}mem_mb = 1024\ncpu = 1\n{extra}\n{}",
            if extra.contains("allow_elevated") {
                ""
            } else {
                "allow_elevated = true\n"
            },
            common::env_path_toml()
        ),
    )
    .unwrap();
}

async fn tags_of_r1(layout: &Layout) -> Vec<String> {
    let client = hub(layout).await.unwrap();
    let (per, _) = fetch_runners(&client, None).await.unwrap();
    client.close().await;
    per.iter()
        .flat_map(|(_, r)| r)
        .find(|r| r.name.ends_with("r1"))
        .map(|r| r.caps.tags.iter().cloned().collect())
        .unwrap_or_default()
}

async fn until_tag(layout: &Layout, tag: &str) {
    for _ in 0..150 {
        if tags_of_r1(layout).await.iter().any(|t| t == tag) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the broker never saw tag {tag}");
}

fn only_runner(answers: Vec<(String, Outcome)>) -> Outcome {
    assert_eq!(answers.len(), 1, "{answers:?}");
    answers.into_iter().next().unwrap().1
}

#[test]
fn diff_names_the_settings_that_need_a_restart() {
    let host = lr_broker::policy::HostResources {
        mem_mb: 4096,
        cpu: 4,
    };
    let text = "allow_elevated = true\n[env]\npath = \"x\"\n";
    let parse = |name: &str, text: &str| {
        RunnerConfig::parse(text, Some(name), name, "/state".into(), host).unwrap()
    };
    let old = parse("a", text);
    assert!(lr_cli::runner_reload::diff(&old, &parse("a", text)).is_empty());
    assert_eq!(
        lr_cli::runner_reload::diff(&old, &parse("b", text)),
        ["name"]
    );
    let other = text.replace("allow_elevated = true", "allow_elevated = false");
    assert_eq!(
        lr_cli::runner_reload::diff(&old, &parse("a", &other)),
        ["allow_elevated"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_with_no_role_running_exits_1() {
    let dir = TempDir::new("reload-none");
    let layout = layout(&dir);
    assert_eq!(reload(&layout, None, None).await, Ok(1));
    assert_eq!(
        only_runner(
            reload_all(&layout, Some(RoleArg::Runner), Some("r1".into()))
                .await
                .unwrap()
        ),
        Outcome::NotRunning
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_takes_its_new_settings_and_keeps_the_old_ones_on_errors() {
    let dir = TempDir::new("reload-runner");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_roles = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;
    let only_r1 = || reload_all(&layout, Some(RoleArg::Runner), Some("r1".into()));

    write_runner_toml(&layout, "tags = [\"gpu\"]");
    assert_eq!(
        only_runner(only_r1().await.unwrap()),
        Outcome::Reloaded {
            restart_required: Vec::new()
        }
    );
    until_tag(&layout, "gpu").await;

    std::fs::write(
        layout.runner_dir("r1").join("runner.toml"),
        "tags = = broken\n",
    )
    .unwrap();
    match only_runner(only_r1().await.unwrap()) {
        Outcome::Refused(why) => assert!(!why.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(tags_of_r1(&layout).await, ["gpu"], "the old settings stay");

    write_runner_toml(
        &layout,
        "tags = [\"gpu\"]\n[programs.no-such-program-lr-test]\n",
    );
    match only_runner(only_r1().await.unwrap()) {
        Outcome::Refused(why) => assert!(why.contains("no-such-program-lr-test"), "{why}"),
        other => panic!("programs are checked again: {other:?}"),
    }

    write_runner_toml(&layout, "allow_elevated = false\ntags = [\"cpu\"]");
    assert_eq!(
        only_runner(only_r1().await.unwrap()),
        Outcome::Reloaded {
            restart_required: vec!["allow_elevated".into()]
        }
    );
    until_tag(&layout, "cpu").await;
    assert_eq!(
        only_runner(only_r1().await.unwrap()),
        Outcome::Reloaded {
            restart_required: vec!["allow_elevated".into()]
        },
        "the running value is still the old one, so it is still reported"
    );

    runners.stop();
    hub_roles.stop();
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn role_and_name_select_the_targets() {
    let dir = TempDir::new("reload-select");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_roles = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    until_ready(&layout).await;
    let names = |answers: Vec<(String, Outcome)>| -> Vec<String> {
        answers.into_iter().map(|(n, _)| n).collect()
    };
    assert_eq!(
        names(reload_all(&layout, None, None).await.unwrap()),
        ["r1", "hub", "broker"]
    );
    assert_eq!(
        names(reload_all(&layout, Some(RoleArg::Hub), None).await.unwrap()),
        ["hub"]
    );
    assert_eq!(
        names(
            reload_all(&layout, Some(RoleArg::Runner), None)
                .await
                .unwrap()
        ),
        ["r1"]
    );
    assert_eq!(
        names(
            reload_all(&layout, None, Some("broker".into()))
                .await
                .unwrap()
        ),
        ["broker"]
    );
    for (who, outcome) in reload_all(&layout, Some(RoleArg::All), None).await.unwrap() {
        assert_eq!(
            outcome,
            Outcome::Reloaded {
                restart_required: Vec::new()
            },
            "{who}"
        );
    }
    std::fs::write(
        layout.config_file("hub-policy.toml"),
        "[brokers]\nstrategy = \"x\"\n",
    )
    .unwrap();
    assert_eq!(reload(&layout, Some(RoleArg::Hub), None).await, Ok(1));
    runners.stop();
    hub_roles.stop();
    broker.stop();
}
