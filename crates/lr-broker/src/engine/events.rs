use super::{Engine, Role};
use crate::api::{access, mask_event, topic_of, Access, Viewer};
use lr_proto::{BrokerToHub, EventMsg, QueueEvent};
use tokio::sync::broadcast;

/// A change on the broker's bus: the event and the client key of the run it is about (`None`
/// for runner events). Subscribers filter it by the visibility policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusEvent {
    /// Client key of the run, `None` for runner and service events.
    pub client: Option<String>,
    /// What happened.
    pub event: QueueEvent,
}

/// Where the broker's bus can be subscribed to, also while [`crate::Broker::run`] runs: it
/// holds no subscription itself, each [`BusEvents::subscribe`] receives changes from that
/// moment on.
#[derive(Clone)]
pub struct BusEvents(pub(crate) broadcast::Sender<BusEvent>);

impl BusEvents {
    /// Receives every change of jobs and runners from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.0.subscribe()
    }
}

impl Engine {
    pub(crate) fn publish(&self, client: Option<&str>, event: QueueEvent) {
        let _ = self.bus.send(BusEvent {
            client: client.map(str::to_string),
            event: event.clone(),
        });
        let topic = topic_of(&event);
        let others = self.policy.file().visibility.others_orders;
        for (&id, conn) in &self.conns {
            let Role::Hub(c) = &conn.role else {
                continue;
            };
            let viewer = Viewer {
                key: c.key.clone(),
                owner: c.owner,
            };
            let level = client.map_or(Access::Owner, |k| access(&viewer, k, others));
            let Some(masked) = mask_event(event.clone(), level) else {
                continue;
            };
            for (&sub, topics) in &conn.subs {
                if topics.contains(&topic) {
                    self.send(
                        id,
                        &BrokerToHub::Event(EventMsg {
                            sub,
                            event: masked.clone(),
                        }),
                    );
                }
            }
        }
    }
}
