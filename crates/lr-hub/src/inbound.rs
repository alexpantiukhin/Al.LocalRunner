use crate::engine::{refusal, BrokerEvent, Engine, LOCAL_BROKER};
use crate::limits::Overflow;
use crate::track::Stage;
use lr_abstractions::{Frame, Level, StoreError};
use lr_core::{JobId, JobState};
use lr_proto::{
    Ack, BrokerToHub, CancelOutcome, CliReply, HubToBroker, ProtocolErrorCode, ReasonCode, Resume,
    ResumePoint,
};

const RESEND_AFTER_S: u64 = 5;

fn sequenced(m: &BrokerToHub) -> Option<(JobId, u64)> {
    match m {
        BrokerToHub::Started(x) => Some((x.job.clone(), x.seq)),
        BrokerToHub::Chunk(x) => Some((x.job.clone(), x.seq)),
        BrokerToHub::StepResult(x) => Some((x.job.clone(), x.seq)),
        BrokerToHub::JobResult(x) => Some((x.job.clone(), x.seq)),
        BrokerToHub::Returned(x) => Some((x.job.clone(), x.seq)),
        BrokerToHub::CancelAck(x) if x.seq > 0 => Some((x.job.clone(), x.seq)),
        _ => None,
    }
}

impl Engine {
    fn send_now(&mut self, name: &str, msg: &HubToBroker) {
        let Some(link) = self.links.get_mut(name) else {
            return;
        };
        let Some(out) = &link.out else {
            return;
        };
        let sent = Frame::json(msg).is_ok_and(|f| out.try_send(f).is_ok());
        if !sent {
            self.s.overflow(
                Overflow::BrokerQueue,
                Level::Warn,
                "send queue to broker full, reconnecting",
                &[("broker", name)],
            );
            link.close();
        }
    }

    fn is_current(&self, job: &JobId, name: &str) -> bool {
        self.tracks
            .get(job)
            .is_some_and(|t| t.order.broker.as_deref() == Some(name))
    }

    pub(crate) async fn on_broker(&mut self, name: &str, ev: BrokerEvent) {
        match ev {
            BrokerEvent::Connected { zone, out, kill } => {
                let link = self.links.entry(name.to_string()).or_default();
                link.out = Some(out);
                link.kill = Some(kill);
                link.sent.clear();
                link.refused = None;
                if let Some(z) = zone {
                    link.zone = Some(z);
                    if name != LOCAL_BROKER {
                        if let Err(e) = self.s.store.set_zone(name, z).await {
                            self.s.diag(
                                Level::Error,
                                "storage failed: broker zone not recorded",
                                &[("broker", name), ("error", &e.to_string())],
                            );
                        }
                    }
                }
                let points = self
                    .tracks
                    .values()
                    .filter(|t| t.order.broker.as_deref() == Some(name))
                    .map(|t| ResumePoint {
                        job: t.order.job.clone(),
                        last_seq: t.order.last_seq,
                    })
                    .collect();
                self.send_now(name, &HubToBroker::Resume(Resume { points }));
                self.flush(name).await;
                let waiting: Vec<JobId> = self
                    .tracks
                    .iter()
                    .filter(|(_, t)| t.stage == Stage::Waiting)
                    .map(|(j, _)| j.clone())
                    .collect();
                for job in waiting {
                    self.start_selection(&job);
                }
                crate::bus::reread(&self.s, name);
            }
            BrokerEvent::Disconnected => {
                if let Some(link) = self.links.get_mut(name) {
                    link.out = None;
                    link.kill = None;
                    link.sent.clear();
                }
                crate::bus::reread(&self.s, name);
            }
            BrokerEvent::Refused(e) => {
                if let Some(link) = self.links.get_mut(name) {
                    link.refused = Some(e);
                }
                if name == LOCAL_BROKER {
                    self.local_user_refused().await;
                }
            }
            BrokerEvent::Message(m) => self.on_message(name, *m).await,
        }
    }

    async fn on_message(&mut self, name: &str, m: BrokerToHub) {
        if let Some((job, seq)) = sequenced(&m) {
            return self.on_sequenced(name, job, seq, m).await;
        }
        match m {
            BrokerToHub::Accepted(a) if self.is_current(&a.job, name) => {
                self.ack_sent(name, &a.job, a.order_seq).await;
                let mut trimmed = None;
                if let Some(t) = self.tracks.get_mut(&a.job) {
                    if t.stage == Stage::Sent {
                        t.stage = Stage::Accepted;
                        let asked = t.spec.as_ref().map(|s| s.requirements.timeout_s);
                        trimmed = a.timeout_trimmed_to.map(|to| (asked, to));
                    }
                    t.wait_reason = a.waiting;
                }
                if let Some((asked, to)) = trimmed {
                    let line = crate::logs::trimmed_line(asked, to);
                    self.log_copy(&a.job, &line).await;
                    let reply = CliReply::Output {
                        job: a.job.clone(),
                        text: line,
                    };
                    self.notify(&a.job, &reply);
                }
            }
            BrokerToHub::Rejected(r) if self.is_current(&r.job, name) => {
                self.rebroker(&r.job, name, r.code, r.detail).await;
            }
            BrokerToHub::CancelAck(c) if self.is_current(&c.job, name) => {
                let never_accepted = self
                    .tracks
                    .get(&c.job)
                    .is_some_and(|t| t.cancelled && !t.stage.accepted());
                if never_accepted {
                    let reason = Some(ReasonCode::CancelledByUser);
                    return self
                        .final_result(&c.job, JobState::Cancelled, None, reason)
                        .await;
                }
                self.cancel_answered(name, &c.job, c.cancel_seq, c.outcome, c.denial.as_deref())
                    .await;
            }
            BrokerToHub::Ack(a) if self.is_current(&a.job, name) => {
                self.ack_sent(name, &a.job, a.seq).await;
            }
            BrokerToHub::Resume(r) => {
                for p in &r.points {
                    if self.is_current(&p.job, name) {
                        self.ack_sent(name, &p.job, p.last_seq).await;
                    }
                }
                if let Some(link) = self.links.get_mut(name) {
                    link.sent.clear();
                }
                self.flush(name).await;
            }
            BrokerToHub::Error(e) => {
                self.s.diag(
                    Level::Warn,
                    "broker reported an error",
                    &[("broker", name), ("error", &e.message)],
                );
                if e.code == ProtocolErrorCode::Unavailable {
                    let at = self.now().plus_secs(RESEND_AFTER_S);
                    if let Some(link) = self.links.get_mut(name) {
                        link.resend_at = Some(at);
                    }
                }
            }
            _ => {}
        }
    }

    async fn ack_sent(&mut self, name: &str, job: &JobId, seq: u64) {
        let Some(top) = self.links.get_mut(name).map(|l| l.acked(job, seq)) else {
            return;
        };
        if top == 0 {
            return;
        }
        if let Err(e) = self.s.store.ack_outbox(job, top).await {
            self.store_failed(
                "storage failed: acknowledged entry stays in outbox",
                job,
                &e,
            );
        }
    }

    async fn cancel_answered(
        &mut self,
        name: &str,
        job: &JobId,
        cancel_seq: u64,
        outcome: CancelOutcome,
        denial: Option<&str>,
    ) {
        if outcome == CancelOutcome::Denied {
            let text = format!("=== cancel denied: {} ===\n", denial.unwrap_or("-"));
            self.log_copy(job, &text).await;
        }
        self.ack_sent(name, job, cancel_seq).await;
    }

    async fn on_sequenced(&mut self, name: &str, job: JobId, seq: u64, m: BrokerToHub) {
        let ack = HubToBroker::Ack(Ack {
            job: job.clone(),
            seq,
        });
        if !self.is_current(&job, name) {
            return self.send_now(name, &ack);
        }
        match self.s.store.accept_seq(&job, seq).await {
            Ok(true) => {
                if let Some(t) = self.tracks.get_mut(&job) {
                    t.order.last_seq = seq;
                }
                self.apply(name, &job, m).await;
                self.send_now(name, &ack);
            }
            Ok(false) | Err(StoreError::NotFound(_)) => self.send_now(name, &ack),
            Err(StoreError::SeqGap { expected, got }) => {
                self.s.diag(
                    Level::Warn,
                    "seq gap from broker, reconnecting",
                    &[
                        ("broker", name),
                        ("job", &job.0),
                        ("expected", &expected.to_string()),
                        ("got", &got.to_string()),
                    ],
                );
                let e = refusal(
                    ProtocolErrorCode::SeqGap,
                    format!("job {job}: expected seq {expected}, got {got}"),
                );
                self.send_now(name, &HubToBroker::Error(e));
                if let Some(link) = self.links.get_mut(name) {
                    link.close();
                }
            }
            Err(e) => self.store_failed("storage failed: broker message not recorded", &job, &e),
        }
    }

    async fn apply(&mut self, name: &str, job: &JobId, m: BrokerToHub) {
        match m {
            BrokerToHub::Started(s) => {
                if let Some(t) = self.tracks.get_mut(job) {
                    t.stage = Stage::Started;
                    t.runner = Some(s.runner.clone());
                }
                let line = crate::logs::started_line(&s.runner, s.started_at);
                self.log_copy(job, &line).await;
            }
            BrokerToHub::Chunk(c) => {
                self.log_copy(job, &c.text).await;
                let reply = CliReply::Output {
                    job: job.clone(),
                    text: c.text,
                };
                self.notify(job, &reply);
            }
            BrokerToHub::StepResult(r) => {
                let line = crate::logs::step_line(&r);
                self.log_copy(job, &line).await;
            }
            BrokerToHub::JobResult(r) => {
                if let Some(d) = &r.detail {
                    self.log_copy(job, &format!("=== {d} ===\n")).await;
                }
                self.final_result(job, r.state, r.exit_code, r.reason).await;
            }
            BrokerToHub::Returned(r) => {
                let detail = r.detail.unwrap_or_else(|| r.code.to_string());
                self.rebroker(job, name, r.code, detail).await;
            }
            BrokerToHub::CancelAck(c) => {
                self.cancel_answered(name, job, c.cancel_seq, c.outcome, c.denial.as_deref())
                    .await;
            }
            _ => {}
        }
    }
}
