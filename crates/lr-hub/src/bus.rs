//! The hub's event bus (spec 9.3, 13): on every broker connection the hub subscribes to the
//! queue and runner events, which the broker masks by its visibility policy for this hub's
//! key, and relays them to the CLI connections that asked for [`CliRequest::Events`]
//! (the dashboard's SSE).
//!
//! [`CliRequest::Events`]: lr_proto::CliRequest::Events

use crate::engine::Shared;
use lr_abstractions::Frame;
use lr_proto::{CliReply, DashboardEvent, HubToBroker, QueueEvent, ReadCall, ReadRequest, Topic};
use tokio::sync::{broadcast, mpsc};

const BUS_EVENTS: usize = 1024;

/// Name under which the hub itself reports lost events.
pub(crate) const HUB: &str = "hub";

pub(crate) fn new_bus() -> broadcast::Sender<DashboardEvent> {
    broadcast::channel(BUS_EVENTS).0
}

/// The subscription a broker connection opens with.
pub(crate) fn subscription(shared: &Shared) -> Option<Frame> {
    let read = HubToBroker::Read(ReadCall {
        req: shared.next_req(),
        request: ReadRequest::Subscribe {
            topics: vec![Topic::Queue, Topic::Runners],
        },
    });
    Frame::json(&read).ok()
}

/// An event of broker `broker` for every CLI connection that watches.
pub(crate) fn publish(shared: &Shared, broker: &str, event: QueueEvent) {
    let _ = shared.bus.send(DashboardEvent {
        broker: broker.to_string(),
        event,
    });
}

/// Tells the watching connections to reread the state of broker `broker`: its connection
/// came or went, so events it sent meanwhile may be missing.
pub(crate) fn reread(shared: &Shared, broker: &str) {
    publish(shared, broker, QueueEvent::Unknown);
}

/// Answers [`CliReply::Watching`] on `reply`, then relays the bus there until the connection
/// closes; events the connection could not take in time come as one `Unknown` event of the
/// hub, so the reader rereads the state.
pub(crate) fn watch(shared: &Shared, reply: mpsc::Sender<CliReply>) {
    let mut rx = shared.bus.subscribe();
    tokio::spawn(async move {
        if reply.send(CliReply::Watching).await.is_err() {
            return;
        }
        loop {
            let event = tokio::select! {
                r = rx.recv() => match r {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => DashboardEvent {
                        broker: HUB.to_string(),
                        event: QueueEvent::Unknown,
                    },
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = reply.closed() => return,
            };
            if reply.send(CliReply::Event(event)).await.is_err() {
                return;
            }
        }
    });
}
