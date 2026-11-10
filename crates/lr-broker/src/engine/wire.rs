use super::conn::{ConnId, Out};
use super::Engine;
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
            let _ = c.outlet.kill.send(true);
            let total = self.slow_peers.fetch_add(1, Ordering::Relaxed) + 1;
            self.diag(
                Level::Warn,
                "slow peer disconnected: its outgoing queue is full",
                &[
                    ("conn", &conn.to_string()),
                    ("peer", &format!("{:?}", c.peer.endpoint)),
                    ("slow_peers_total", &total.to_string()),
                ],
            );
        }
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
