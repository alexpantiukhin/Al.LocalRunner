mod common;

use common::TempDir;
use lr_cli::commands::down_cmd::down;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::view::{known_brokers, read};
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners};
use lr_core::JobState;
use lr_proto::{
    AdminReply, AdminRequest, CliReply, CliRequest, IpcChannel, ReadReply, ReadRequest,
    SubmitRequest,
};
use std::time::Duration;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy =
        format!("[listen]\naddress = \"127.0.0.1:0\"\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    let runner = root.join("runner").join("r1");
    std::fs::create_dir_all(&runner).unwrap();
    std::fs::write(
        runner.join("runner.toml"),
        format!(
            "allow_elevated = true\nallow_same_user_as_broker = true\nmem_mb = 1024\ncpu = 1\n{}",
            common::env_path_toml()
        ),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn all_roles_start_from_wiring_on_loopback() {
    let dir = TempDir::new("wiring");
    let layout = layout(&dir);

    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    assert_eq!(runners.runners, ["r1"]);

    let local = eventually("the local broker to be available", || async {
        let client = hub(&layout).await.ok()?;
        let brokers = known_brokers(&client).await.ok()?;
        client.close().await;
        brokers.into_iter().find(|b| b.available).map(|b| b.name)
    })
    .await;

    let runner = eventually("runner r1 online", || {
        let local = local.clone();
        let layout = layout.clone();
        async move {
            let client = hub(&layout).await.ok()?;
            let reply = read(&client, &local, ReadRequest::Runners).await.ok()?;
            client.close().await;
            match reply {
                ReadReply::Runners { runners } => runners
                    .into_iter()
                    .find(|r| r.online && r.name.ends_with("r1")),
                _ => None,
            }
        }
    })
    .await;
    assert_eq!((runner.caps.mem_mb, runner.caps.cpu), (1024, 1));
    assert!(runner.caps.disk_mb > 0, "free disk is measured");

    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".localrunner")).unwrap();
    std::fs::write(
        repo.join(".localrunner").join("profiles.toml"),
        "[profile.unit]
command = \"echo wired\"
timeout_s = 60
mem_mb = 256
",
    )
    .unwrap();
    common::init_repo(&repo);
    common::git(&repo, &["add", "."]);
    common::git(&repo, &["commit", "-q", "-m", "profiles"]);
    let identity = common::session("wiring", None, &repo).expect("identity");
    let client = hub(&layout).await.expect("hub channel");
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
    let job = match client
        .call::<_, CliReply>(&submit)
        .await
        .expect("submitted")
    {
        CliReply::Submitted { job, .. } => job,
        other => panic!("submit answered {other:?}"),
    };
    let mut output = String::new();
    let state = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match client.recv::<CliReply>().await.expect("hub answer") {
                CliReply::Output { text, .. } => output.push_str(&text),
                CliReply::Exit { job: j, state, .. } if j == job => return state,
                CliReply::Rejected { code, detail } => panic!("rejected {code}: {detail}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the job finishes");
    client.close().await;
    assert_eq!(state, JobState::Passed, "output: {output}");
    assert!(output.contains("wired"), "output: {output}");

    let admin = IpcClient::connect(&layout.admin_ipc(), IpcChannel::Admin)
        .await
        .expect("admin channel");
    let add = AdminRequest::BrokerAdd {
        name: Some("laptop".into()),
        label: None,
        runners: None,
        address: None,
    };
    let reply: AdminReply = admin.call(&add).await.expect("admin reply");
    admin.close().await;
    let AdminReply::JoinCode { address, .. } = reply else {
        panic!("broker add answered {reply:?}");
    };
    let bound = broker.network_addresses();
    assert_eq!(bound.len(), 1, "one loopback listener");
    assert!(bound[0].ip().is_loopback());
    assert!(address.contains(&bound[0].port().to_string()), "{address}");
    tokio::net::TcpStream::connect(bound[0])
        .await
        .expect("the TLS listener accepts TCP");

    assert_eq!(down(&layout, vec!["r1".into()], false).await, Ok(0));
    let exits = tokio::time::timeout(Duration::from_secs(15), runners.wait())
        .await
        .expect("the runner stops after down");
    assert_eq!(exits.len(), 1);

    hub_role.stop();
    broker.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_on_a_machine_without_a_broker_lists_no_local_broker() {
    let dir = TempDir::new("nobroker");
    let layout = layout(&dir);
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let client = eventually("the hub channel", || async { hub(&layout).await.ok() }).await;
    let brokers = known_brokers(&client).await.expect("brokers");
    client.close().await;
    assert!(
        brokers.iter().all(|b| b.name != "local"),
        "no broker is configured here, so the hub lists none as local: {brokers:?}"
    );
    assert!(brokers.is_empty(), "{brokers:?}");
    hub_role.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roles_without_logging_toml_have_no_network_log_receiver() {
    use lr_obs::queue::SinkKind;
    let dir = TempDir::new("nosink");
    let layout = layout(&dir);
    assert!(!layout.config_file("logging.toml").exists());
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let hub_role = start_hub(&layout, false).await.expect("hub starts");
    let runners = start_runners(&layout, Some("r1"), 1, None, false)
        .await
        .expect("runner starts");
    for (name, role) in [
        ("broker", &broker),
        ("hub", &hub_role),
        ("runner", &runners),
    ] {
        let kinds = role.sink_kinds();
        assert!(!kinds.is_empty(), "{name} logs somewhere");
        assert!(
            !kinds.contains(&SinkKind::Otlp),
            "{name} has no network receiver by default: {kinds:?}"
        );
    }
    runners.stop();
    hub_role.stop();
    broker.stop();
}
