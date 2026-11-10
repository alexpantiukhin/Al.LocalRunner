use lr_abstractions::{Clock, Frame, Peer, Transport};
use lr_proto::{HEARTBEAT_INTERVAL_S, MISSED_HEARTBEATS_OFFLINE};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub(crate) type ConnId = u64;

pub(crate) enum Out {
    Frame(Frame),
    Close,
}

pub(crate) enum Command {
    Opened {
        conn: ConnId,
        peer: Peer,
        out: mpsc::UnboundedSender<Out>,
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

pub(crate) fn spawn(
    id: ConnId,
    transport: Box<dyn Transport>,
    cmds: mpsc::UnboundedSender<Command>,
    clock: Arc<dyn Clock>,
) {
    let t: Arc<dyn Transport> = Arc::from(transport);
    let (out, mut rx) = mpsc::unbounded_channel();
    if cmds
        .send(Command::Opened {
            conn: id,
            peer: t.peer().clone(),
            out,
        })
        .is_err()
    {
        return;
    }
    let writer = t.clone();
    let beat_clock = clock.clone();
    tokio::spawn(async move {
        let mut next = beat_clock.now().plus_secs(HEARTBEAT_INTERVAL_S);
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(Out::Frame(f)) => {
                        if writer.send(f).await.is_err() {
                            break;
                        }
                    }
                    Some(Out::Close) | None => {
                        writer.close().await;
                        break;
                    }
                },
                _ = beat_clock.sleep_until(next) => {
                    next = next.plus_secs(HEARTBEAT_INTERVAL_S);
                    if writer.send(heartbeat()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        let silence =
            Duration::from_secs(HEARTBEAT_INTERVAL_S * u64::from(MISSED_HEARTBEATS_OFFLINE));
        loop {
            tokio::select! {
                r = t.recv() => match r {
                    Ok(frame) => {
                        if cmds.send(Command::Frame { conn: id, frame }).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                _ = clock.sleep(silence) => {
                    t.close().await;
                    break;
                }
            }
        }
        let _ = cmds.send(Command::Closed { conn: id });
    });
}
