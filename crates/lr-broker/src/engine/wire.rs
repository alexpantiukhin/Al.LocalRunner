use super::conn::{ConnId, Out};
use super::{Engine, BACKLOG_MAX_FRAMES, OUT_QUEUE_FRAMES, REPLAY_WINDOW};
use lr_abstractions::{Frame, Level, Record, Role};
use lr_proto::{BrokerToHub, ProtocolError, ProtocolErrorCode};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc::error::TrySendError;

impl Engine {
    pub(crate) fn diag(&self, level: Level, message: &str, fields: &[(&str, &str)]) {
        let mut record = Record::new(self.now(), level, Role::Broker, "engine", message);
        for (name, value) in fields {
            record = record.field(name, value);
        }
        self.log.write(&record);
    }

    pub(crate) fn send<M: Serialize>(&self, conn: ConnId, msg: &M) {
        if let Ok(frame) = Frame::json(msg) {
            self.send_frame(conn, frame);
        }
    }

    pub(crate) fn send_frame(&self, conn: ConnId, frame: Frame) {
        self.push(conn, Out::Frame(frame));
    }

    pub(crate) fn close(&self, conn: ConnId) {
        self.push(conn, Out::Close);
    }

    fn push(&self, conn: ConnId, out: Out) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if *c.outlet.kill.borrow() {
            return;
        }
        if let Err(TrySendError::Full(_)) = c.outlet.out.try_send(out) {
            self.slow(conn, "its outgoing queue is full");
        }
    }

    fn slow(&self, conn: ConnId, why: &str) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let _ = c.outlet.kill.send(true);
        let total = self.slow_peers.fetch_add(1, Ordering::Relaxed) + 1;
        self.diag(
            Level::Warn,
            &format!("slow peer disconnected: {why}"),
            &[
                ("conn", &conn.to_string()),
                ("peer", &format!("{:?}", c.peer.endpoint)),
                ("slow_peers_total", &total.to_string()),
            ],
        );
    }

    pub(crate) fn send_ordered(&mut self, conn: ConnId, frames: impl IntoIterator<Item = Frame>) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        c.backlog.extend(frames);
        if c.backlog.len() > BACKLOG_MAX_FRAMES {
            c.backlog.clear();
            return self.slow(conn, "its replay backlog is full");
        }
        self.pump(conn);
    }

    pub(crate) fn pump(&mut self, conn: ConnId) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        if *c.outlet.kill.borrow() {
            c.backlog.clear();
            return;
        }
        while OUT_QUEUE_FRAMES - c.outlet.out.capacity() < REPLAY_WINDOW {
            let Some(frame) = c.backlog.pop_front() else {
                break;
            };
            if let Err(e) = c.outlet.out.try_send(Out::Frame(frame)) {
                if let TrySendError::Full(Out::Frame(f)) = e {
                    c.backlog.push_front(f);
                }
                break;
            }
        }
        c.outlet
            .drain
            .store(!c.backlog.is_empty(), Ordering::SeqCst);
    }

    pub(crate) fn error(&self, conn: ConnId, code: ProtocolErrorCode, message: String) {
        self.send(
            conn,
            &BrokerToHub::Error(ProtocolError {
                code,
                message,
                supported: None,
            }),
        );
    }

    pub(crate) fn refuse(&self, conn: ConnId, code: ProtocolErrorCode, message: String) {
        self.error(conn, code, message);
        self.close(conn);
    }
}
