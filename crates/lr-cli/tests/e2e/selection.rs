use crate::common::TempDir;
use crate::kit::*;
use crate::support::*;
use lr_cli::commands::join_cmd::hub_join;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, Running};
use lr_core::{JobState, Zone};
use lr_proto::{ReasonCode, RejectReason};
use std::time::Duration;

const LAN: &str = "[networks]\nhere = [\"127.0.0.0/8\", \"::1/128\"]\n";
const ENDS: Duration = Duration::from_secs(120);

async fn zone_of_main(layout: &Layout, zone: Zone) {
    eventually(
        &format!("the broker main in zone {}", zone.as_str()),
        ENDS,
        || async {
            brokers(layout)
                .await
                .iter()
                .any(|b| b.name == "main" && b.available && b.zone == Some(zone))
                .then_some(())
        },
    )
    .await;
}

async fn restart_hub(layout: &Layout, old: Running) -> Running {
    old.stop();
    eventually(
        "the hub to start again",
        Duration::from_secs(30),
        || async { start_hub(layout, false).await.ok() },
    )
    .await
}

fn remote_profile(name: &str, text: &str, extra: &str) -> String {
    echo_profile(
        name,
        text,
        120,
        &format!("platforms = [\"{PLATFORM}\"]\n{extra}"),
    )
}

// scenario: 16.8
// scenario: 16.13
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zone_limited_order_without_an_admissible_broker_is_refused_with_figures() {
    let dir = TempDir::new("e2e-zone");
    let (main, mut roles) = machine(&dir, "main", LAN, &["r1"], &platform_runner_toml()).await;
    let (laptop, mut laptop_roles) = machine(&dir, "laptop", "", &[], "").await;
    let (address, code) = join_code(&main, Some("laptop"), None, None).await;
    assert_eq!(
        hub_join(&laptop, &address, &code, Some("main".into())).await,
        Ok(0)
    );
    let laptop_hub = restart_hub(&laptop, laptop_roles.take_hub()).await;
    zone_of_main(&laptop, Zone::Lan).await;

    let profiles = format!(
        "{}\n{}\n{}",
        remote_profile(
            "lan-only",
            "inside-the-lan",
            "mem_mb = 256\nzone = \"lan\"\n"
        ),
        remote_profile("anywhere", "on-any-network", "mem_mb = 256\n"),
        remote_profile("huge", "never-runs", "mem_mb = 12288\n"),
    );
    let repo = repo(&dir, &profiles);
    push_origin(&dir, &repo);

    let (client, job) = submit(&laptop, &repo, "lan-only", Some(PLATFORM)).await;
    let ended = until_exit(client, &job, ENDS).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert!(ended.output.contains("inside-the-lan"), "{}", ended.output);

    let (client, job) = submit(&laptop, &repo, "huge", Some(PLATFORM)).await;
    let (code, detail) = until_rejected(client, &job, ENDS).await;
    assert_eq!(
        code,
        ReasonCode::Rejected(RejectReason::NoMatchingRunner),
        "{detail}"
    );
    assert!(
        detail.contains("нужно 12288 МБ, у раннера 1024"),
        "every broker answered, none fits: the refusal gives the figures: {detail}"
    );

    let file = main.config_file("policy.toml");
    let away = std::fs::read_to_string(&file)
        .unwrap()
        .replace(LAN, "")
        .replace("127.0.0.1:0", &address);
    roles.take_broker().stop();
    eventually("the broker's port to be free", ENDS, || {
        let free = std::net::TcpListener::bind(&address).is_ok();
        async move { free.then_some(()) }
    })
    .await;
    std::fs::write(&file, away).unwrap();
    roles.push(
        start_broker(&main, false)
            .await
            .expect("the broker starts again"),
    );
    zone_of_main(&laptop, Zone::Remote).await;

    let (client, job) = submit(&laptop, &repo, "lan-only", Some(PLATFORM)).await;
    let (code, detail) = until_rejected(client, &job, ENDS).await;
    assert_eq!(
        code,
        ReasonCode::Rejected(RejectReason::NoMatchingRunner),
        "{detail}"
    );
    assert!(
        detail.starts_with("нет подходящего раннера: local:") && !detail.contains("main:"),
        "a lan order is refused without asking the remote broker: {detail}"
    );
    assert!(
        all_history(&main).await.iter().all(|r| r.job != job),
        "the remote broker never saw the lan order"
    );
    let (client, job) = submit(&laptop, &repo, "anywhere", Some(PLATFORM)).await;
    let ended = until_exit(client, &job, ENDS).await;
    assert_eq!(ended.state, JobState::Passed, "{ended:?}");
    assert!(ended.output.contains("on-any-network"), "{}", ended.output);

    laptop_hub.stop();
    laptop_roles.stop();
    roles.stop();
    remove(dir).await;
}
