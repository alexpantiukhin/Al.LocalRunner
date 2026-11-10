use lr_abstractions::{Clock, Frame, Peer, Transport};
use lr_proto::{HEARTBEAT_INTERVAL_S, MISSED_HEARTBEATS_OFFLINE};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

pub(crate) type ConnId = u64;

pub(crate) const COMMAND_QUEUE: usize = 256;

pub(crate) enum Out {
    Frame(Frame),
    Close,
}

pub(crate) struct Outlet {
    pub(crate) out: mpsc::Sender<Out>,
    pub(crate) kill: watch::Sender<bool>,
}

pub(crate) enum Command {
    Opened {
        conn: ConnId,
        peer: Peer,
        outlet: Outlet,
    },
    Frame {
        conn: ConnId,
        frame: Frame,
    },
    Closed {
        conn: ConnId,
    },
}

fn heartbeat() -> Frame {
    Frame::from_bytes(br#"{"type":"heartbeat"}"#.to_vec())
}

async fn killed(kill: &mut watch::Receiver<bool>) {
    while !*kill.borrow_and_update() {
        if kill.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

pub(crate) async fn spawn(
    id: ConnId,
    transport: Box<dyn Transport>,
    cmds: mpsc::Sender<Command>,
    clock: Arc<dyn Clock>,
) {
    let t: Arc<dyn Transport> = Arc::from(transport);
    let (out, mut rx) = mpsc::channel(super::OUT_QUEUE_FRAMES);
    let (kill, kill_rx) = watch::channel(false);
    let opened = Command::Opened {
        conn: id,
        peer: t.peer().clone(),
        outlet: Outlet { out, kill },
    };
    if cmds.send(opened).await.is_err() {
        return;
    }
    let writer = t.clone();
    let beat_clock = clock.clone();
    let mut writer_kill = kill_rx.clone();
    tokio::spawn(async move {
        let mut next = beat_clock.now().plus_secs(HEARTBEAT_INTERVAL_S);
        loop {
            let frame = tokio::select! {
                m = rx.recv() => match m {
                    Some(Out::Frame(f)) => f,
                    Some(Out::Close) | None => {
                        writer.close().await;
                        break;
                    }
                },
                _ = beat_clock.sleep_until(next) => {
                    next = next.plus_secs(HEARTBEAT_INTERVAL_S);
                    heartbeat()
                }
                _ = killed(&mut writer_kill) => break,
            };
            tokio::select! {
                sent = writer.send(frame) => if sent.is_err() { break },
                _ = killed(&mut writer_kill) => break,
            }
        }
    });
    let mut reader_kill = kill_rx;
    tokio::spawn(async move {
        let silence =
            Duration::from_secs(HEARTBEAT_INTERVAL_S * u64::from(MISSED_HEARTBEATS_OFFLINE));
        loop {
            tokio::select! {
                r = t.recv() => match r {
                    Ok(frame) => {
                        if cmds.send(Command::Frame { conn: id, frame }).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                _ = clock.sleep(silence) => {
                    t.close().await;
                    break;
                }
                _ = killed(&mut reader_kill) => {
                    t.close().await;
                    break;
                }
            }
        }
        let _ = cmds.send(Command::Closed { conn: id }).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lr_abstractions::fakes::{channel_pair, ManualClock};
    use lr_abstractions::PeerAccess;
    use lr_core::{Endpoint, Timestamp};

    #[tokio::test]
    async fn reader_waits_for_room_in_the_command_queue_and_loses_nothing() {
        let clock = Arc::new(ManualClock::new(Timestamp(0)));
        let peer = Peer {
            endpoint: Endpoint::Ipc {
                os_user: "alex".into(),
            },
            key: None,
            access: PeerAccess::Local,
        };
        let (client, server) = channel_pair(clock.clone(), peer.clone(), peer);
        let (tx, mut rx) = mpsc::channel(2);
        spawn(1, Box::new(server), tx, clock).await;
        for i in 0..10u8 {
            client
                .send(Frame::from_bytes(vec![b'0' + i]))
                .await
                .unwrap();
        }
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert_eq!(rx.len(), 2);
        let mut got = Vec::new();
        let mut kept = Vec::new();
        while got.len() < 10 {
            match rx.recv().await.expect("connection stays open") {
                Command::Frame { frame, .. } => got.push(frame.as_bytes()[0]),
                other => kept.push(other),
            }
        }
        assert_eq!(got, (0..10u8).map(|i| b'0' + i).collect::<Vec<_>>());
    }
}
