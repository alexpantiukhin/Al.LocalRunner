use lr_cli::commands::jobs::hub;
use lr_cli::commands::read_cmd::fetch_runners;
use lr_cli::commands::view::known_brokers;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_proto::{CliReply, CliRequest, DashboardEvent};
use std::future::Future;
use std::time::Duration;

/// The longest wait for an event a role is about to produce: only an upper bound against a
/// hang.
pub const HANG: Duration = Duration::from_secs(20);

/// The live events of the hub of `layout` (the dashboard's stream).
pub struct HubEvents(IpcClient);

impl HubEvents {
    pub async fn open(layout: &Layout) -> HubEvents {
        let client = hub(layout).await.expect("hub channel");
        match client.call::<_, CliReply>(&CliRequest::Events).await {
            Ok(CliReply::Watching) => HubEvents(client),
            other => panic!("the hub did not open its event stream: {other:?}"),
        }
    }

    /// The next event the hub relays.
    pub async fn next(&mut self) -> DashboardEvent {
        loop {
            match self.0.recv::<CliReply>().await {
                Ok(CliReply::Event(e)) => return e,
                Ok(_) => {}
                Err(e) => panic!("the hub's event stream ended: {e}"),
            }
        }
    }

    pub async fn close(self) {
        self.0.close().await;
    }
}

/// Checks `check` now and again after every event the hub of `layout` relays (a broker's
/// queue or runner event, a broker connection coming or going) until it gives a value.
pub async fn until_hub<T, F, Fut>(layout: &Layout, what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let mut events = HubEvents::open(layout).await;
    let found = async {
        loop {
            if let Some(v) = check().await {
                return v;
            }
            events.next().await;
        }
    };
    let found = tokio::time::timeout(HANG, found).await;
    events.close().await;
    found.unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Waits until the hub of `layout` reaches a broker and a runner whose name ends with `name`
/// is online there.
pub async fn until_online(layout: &Layout, name: &str) {
    until_hub(layout, &format!("runner {name} online"), || async {
        let client = hub(layout).await.ok()?;
        let available = known_brokers(&client)
            .await
            .is_ok_and(|b| b.iter().any(|b| b.available));
        let online = match fetch_runners(&client, None).await {
            Ok((per, _)) => per
                .iter()
                .flat_map(|(_, r)| r)
                .any(|r| r.online && r.name.ends_with(name)),
            Err(_) => false,
        };
        client.close().await;
        (available && online).then_some(())
    })
    .await;
}
