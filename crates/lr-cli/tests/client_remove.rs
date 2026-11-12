mod common;

use common::TempDir;
use lr_abstractions::{Dialer, Frame, HubStore, Transport, TransportError};
use lr_cli::build::KEY_FILE;
use lr_cli::commands::join_cmd::hub_join;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::start_broker;
use lr_net::{ClientKey, TlsDialer};
use lr_proto::{
    AdminReply, AdminRequest, BrokerToHub, Hello, HubToBroker, IpcChannel, PROTOCOL_VERSION,
};
use std::sync::Arc;
use std::time::Duration;

fn broker_layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("broker");
    let config = dir.path().join("broker-config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy =
        format!("[listen]\naddress = \"127.0.0.1:0\"\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

async fn admin(layout: &Layout, req: AdminRequest) -> AdminReply {
    let client = IpcClient::connect(&layout.admin_ipc(), IpcChannel::Admin)
        .await
        .expect("admin channel");
    let reply = client.call(&req).await.expect("admin reply");
    client.close().await;
    reply
}

async fn next_message(t: &dyn Transport) -> Result<BrokerToHub, TransportError> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), t.recv())
            .await
            .expect("the broker answers within 10 s")?;
        let m: BrokerToHub = frame.parse().expect("a broker message");
        if !matches!(m, BrokerToHub::Heartbeat) {
            return Ok(m);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_remove_revokes_the_key_and_drops_the_live_connection() {
    let dir = TempDir::new("client-remove");
    let layout = broker_layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let (address, code) = match admin(
        &layout,
        AdminRequest::BrokerAdd {
            name: Some("ci-hub".into()),
            label: None,
            runners: None,
            address: None,
        },
    )
    .await
    {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    };
    let hub = Layout::new(dir.path().join("hub"), dir.path().join("hub"));
    assert_eq!(hub_join(&hub, &address, &code, None).await, Ok(0));
    let store = lr_store::SqliteHubStore::open(&hub.root.join("hub.db")).unwrap();
    let fingerprint = store.brokers().await.unwrap()[0].fingerprint.clone();
    let key = ClientKey::load(&hub.root.join(KEY_FILE)).unwrap();
    let dialer = TlsDialer::new(Arc::new(key));

    let live = dialer.dial(&address, Some(&fingerprint)).await.unwrap();
    let hello = HubToBroker::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: "ci-hub".into(),
        capabilities: None,
    });
    live.send(Frame::json(&hello).unwrap()).await.unwrap();
    assert!(
        matches!(next_message(&*live).await, Ok(BrokerToHub::Welcome(_))),
        "the member is welcomed before the removal"
    );

    let unknown = admin(
        &layout,
        AdminRequest::ClientRemove {
            name: "nobody".into(),
        },
    )
    .await;
    assert!(
        matches!(&unknown, AdminReply::Failed { message } if message == "no hub named nobody"),
        "{unknown:?}"
    );
    let removed = admin(
        &layout,
        AdminRequest::ClientRemove {
            name: "ci-hub".into(),
        },
    )
    .await;
    assert_eq!(
        removed,
        AdminReply::Ok {
            warnings: Vec::new()
        }
    );
    assert!(
        matches!(next_message(&*live).await, Err(TransportError::Closed)),
        "the engine drops the connection of the revoked key"
    );
    assert_eq!(
        admin(&layout, AdminRequest::ClientList).await,
        AdminReply::Names { names: Vec::new() }
    );
    let again = dialer.dial(&address, Some(&fingerprint)).await;
    let refusal = match again {
        Err(e) => e.to_string(),
        Ok(t) => {
            t.send(Frame::json(&hello).unwrap()).await.ok();
            format!("{:?}", next_message(&*t).await)
        }
    };
    assert!(refusal.contains("revoked"), "{refusal}");
    broker.stop();
}
