use crate::engine::{refusal, Shared};
use lr_abstractions::Frame;
use lr_proto::{BrokerToHub, CliReply, HubToBroker, ProtocolErrorCode, ReadCall, ReadRequest};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub(crate) struct LogRead {
    pub(crate) broker: String,
    pub(crate) out: mpsc::Sender<Frame>,
    pub(crate) request: ReadRequest,
    pub(crate) follow: bool,
    pub(crate) wait: Duration,
}

enum Next {
    Msg(Option<Box<BrokerToHub>>),
    Silent,
    Gone,
}

fn unavailable(text: String) -> CliReply {
    CliReply::Error(refusal(ProtocolErrorCode::Unavailable, text))
}

fn call(req: u64, request: ReadRequest) -> Option<Frame> {
    Frame::json(&HubToBroker::Read(ReadCall { req, request })).ok()
}

fn unfollow(s: &Shared, r: &LogRead, req: u64) {
    if !r.follow {
        return;
    }
    if let Some(f) = call(s.next_req(), ReadRequest::Unfollow { req }) {
        let _ = r.out.try_send(f);
    }
}

pub(crate) fn relay_log(s: Arc<Shared>, r: LogRead, reply: mpsc::Sender<CliReply>) {
    tokio::spawn(async move {
        let req = s.next_req();
        let mut rx = s.pending.stream(req, &r.broker);
        let sent = call(req, r.request.clone()).is_some_and(|f| r.out.try_send(f).is_ok());
        if !sent {
            s.pending.forget_stream(req, &r.broker);
            let _ = reply
                .send(unavailable(format!("broker {} did not answer", r.broker)))
                .await;
            return;
        }
        loop {
            let next = tokio::select! {
                m = rx.recv() => Next::Msg(m.map(Box::new)),
                _ = s.clock.sleep(r.wait), if !r.follow => Next::Silent,
                _ = reply.closed() => Next::Gone,
            };
            let answer = match next {
                Next::Msg(Some(m)) => match *m {
                    BrokerToHub::LogChunk(l) => CliReply::LogChunk {
                        broker: r.broker.clone(),
                        text: l.text,
                        end: l.end,
                    },
                    BrokerToHub::ReadReply(a) => CliReply::Read {
                        broker: r.broker.clone(),
                        reply: a.reply,
                    },
                    _ => continue,
                },
                Next::Msg(None) => {
                    unfollow(&s, &r, req);
                    unavailable(format!(
                        "broker {} disconnected or the log fell behind",
                        r.broker
                    ))
                }
                Next::Silent => unavailable(format!("broker {} did not answer", r.broker)),
                Next::Gone => {
                    unfollow(&s, &r, req);
                    break;
                }
            };
            let last = !matches!(answer, CliReply::LogChunk { end: false, .. });
            if reply.send(answer).await.is_err() {
                unfollow(&s, &r, req);
                break;
            }
            if last {
                break;
            }
        }
        s.pending.forget_stream(req, &r.broker);
    });
}
