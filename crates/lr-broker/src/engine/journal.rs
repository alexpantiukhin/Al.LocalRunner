//! Diagnostic events of the broker (spec 12): connections, orders accepted, refused and issued,
//! the policy rule that holds an order. Only identifiers, labels and reason codes are written:
//! never commands, environment or job output.

use super::conn::ConnId;
use super::{Engine, Job, Role};
use lr_abstractions::Level;
use lr_core::{JobId, Wait};
use lr_proto::{ProtocolErrorCode, ReasonCode, RejectReason};
use std::collections::HashMap;

fn code_name(code: ProtocolErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn rule_of(wait: &Wait) -> Option<&str> {
    match wait {
        Wait::Reserved { rule, .. } | Wait::Quota { rule, .. } => Some(rule),
        _ => None,
    }
}

impl Engine {
    pub(crate) fn note_connected(&self, conn: ConnId) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        match &c.role {
            Role::Hub(client) => self.diag(
                Level::Info,
                "client connected",
                &[("conn", &conn.to_string()), ("client", &client.label)],
            ),
            Role::Runner(name) => self.diag(
                Level::Info,
                "runner connected",
                &[("conn", &conn.to_string()), ("runner", name)],
            ),
            Role::Pending => {}
        }
    }

    pub(crate) fn note_closed(&self, conn: ConnId, role: &Role) {
        match role {
            Role::Hub(client) => self.diag(
                Level::Info,
                "client disconnected",
                &[("conn", &conn.to_string()), ("client", &client.label)],
            ),
            Role::Runner(name) => self.diag(
                Level::Info,
                "runner disconnected",
                &[("conn", &conn.to_string()), ("runner", name)],
            ),
            Role::Pending => {}
        }
    }

    pub(crate) fn note_refused(&self, conn: ConnId, code: ProtocolErrorCode) {
        self.diag(
            Level::Warn,
            "connection refused",
            &[("conn", &conn.to_string()), ("code", &code_name(code))],
        );
    }

    pub(crate) fn note_accepted(&self, job: &Job) {
        self.diag(
            Level::Info,
            "order accepted",
            &[("job", &job.key.job.0), ("client", &job.entry.client.label)],
        );
    }

    pub(crate) fn note_rejected(&self, job: &Job, reason: &RejectReason) {
        let code = ReasonCode::Rejected(reason.clone()).to_string();
        self.diag(
            Level::Info,
            "order rejected",
            &[
                ("job", &job.key.job.0),
                ("client", &job.entry.client.label),
                ("reason", &code),
            ],
        );
    }

    pub(crate) fn note_assigned(&self, job: &Job, runner: &str) {
        self.diag(
            Level::Info,
            "order issued to a runner",
            &[
                ("job", &job.key.job.0),
                ("client", &job.entry.client.label),
                ("runner", runner),
            ],
        );
    }

    pub(crate) fn note_rules(&mut self, waiting: &[(JobId, Wait)]) {
        let mut held = HashMap::new();
        for (id, wait) in waiting {
            let Some(rule) = rule_of(wait) else {
                continue;
            };
            let Some(job) = self.queue.iter().find(|j| &j.internal == id) else {
                continue;
            };
            if self.held_by.get(&job.key).map(String::as_str) != Some(rule) {
                self.diag(
                    Level::Info,
                    "order held by a policy rule",
                    &[
                        ("job", &job.key.job.0),
                        ("client", &job.entry.client.label),
                        ("rule", rule),
                    ],
                );
            }
            held.insert(job.key.clone(), rule.to_string());
        }
        self.held_by = held;
    }
}
