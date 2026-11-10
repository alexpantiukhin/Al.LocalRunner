use super::conn::ConnId;
use super::hello::peer_kind;
use super::{Engine, Job};
use lr_abstractions::{
    Applied, Frame, Inbound, JobKey, OutboxKind, StepUpdate, StoreError, Transition,
};
use lr_core::{pick_next, Assignment, JobId, JobState, QueueState, RunningJob, Timestamp, Wait};
use lr_proto::{
    receive, Ack, Assign, AssignSpec, BrokerToHub, BrokerToRunner, Chunk, JobResult, ProtocolError,
    ProtocolErrorCode, QueueEvent, ReasonCode, Received, RunnerToBroker, StartedNotice, StepResult,
    StepView,
};
use serde_json::Value;

pub(crate) fn with_seq(payload: &str, seq: u64) -> Option<Frame> {
    let mut v: Value = serde_json::from_str(payload).ok()?;
    v.as_object_mut()?.insert("seq".into(), seq.into());
    Frame::json(&v).ok()
}

impl Engine {
    pub(crate) fn wait_text(&self, wait: &Wait) -> String {
        match wait.clone() {
            Wait::Exclusive { tag, holder } => Wait::Exclusive {
                tag,
                holder: self.public_id(&holder),
            },
            Wait::HeadReserved { head, at } => Wait::HeadReserved {
                head: self.public_id(&head),
                at,
            },
            other => other,
        }
        .to_string()
    }

    pub(crate) fn running_jobs(&self) -> Vec<RunningJob> {
        self.active
            .iter()
            .map(|j| RunningJob {
                entry: j.entry.clone(),
                runner: j.runner.clone().unwrap_or_default(),
                started_at: j.started_at.unwrap_or(j.assigned_at),
            })
            .collect()
    }

    pub(crate) async fn plan(&mut self) {
        let now = self.now();
        let entries: Vec<_> = self.queue.iter().map(|j| j.entry.clone()).collect();
        let runners = self.runner_views();
        let running = self.running_jobs();
        let plan = pick_next(
            self.policy.policy(),
            &self.site,
            &QueueState {
                queue: &entries,
                runners: &runners,
                running: &running,
                served: &self.served,
            },
            now,
        );
        self.waits = plan
            .waiting
            .iter()
            .filter_map(|(id, wait)| {
                let key = self.queue.iter().find(|j| &j.internal == id)?.key.clone();
                Some((key, self.wait_text(wait)))
            })
            .collect();
        for a in plan.assignments {
            self.assign(a, now).await;
        }
    }

    async fn assign(&mut self, a: Assignment, now: Timestamp) {
        let Some(i) = self.queue.iter().position(|j| j.internal == a.job) else {
            return;
        };
        let Some(conn) = self.runner_conn(&a.runner) else {
            return;
        };
        if self
            .store
            .grant(&self.queue[i].key, &a.runner)
            .await
            .is_err()
        {
            return;
        }
        let mut job = self.queue.remove(i);
        let seq = self.store.next_seq_to_runner(&job.key).await.unwrap_or(1);
        let spec = &job.spec;
        self.send(
            conn,
            &BrokerToRunner::Assign(Assign {
                job: job.key.job.clone(),
                seq,
                spec: Box::new(AssignSpec {
                    client: job.key.client.clone(),
                    project: spec.identity.project.clone(),
                    requirements: job.entry.order.requirements.clone(),
                    steps: spec.steps.clone(),
                    secrets: job
                        .entry
                        .order
                        .requirements
                        .secrets
                        .iter()
                        .cloned()
                        .collect(),
                    code: spec.code.clone(),
                    broker_lost_kill_s: Some(self.lost_kill_s()),
                }),
            }),
        );
        let group = self.policy.policy().group_of(&job.entry.client);
        self.served.record(&group, now);
        job.runner = Some(a.runner);
        job.assigned_at = now;
        self.waits.remove(&job.key);
        self.active.push(job);
    }

    pub(crate) async fn notify_hub(
        &mut self,
        client: &str,
        job: &JobId,
        kind: OutboxKind,
        msg: &BrokerToHub,
    ) {
        let Ok(payload) = serde_json::to_string(msg) else {
            return;
        };
        let limit = self.outbox_limit();
        let pushed = self
            .store
            .push_result(client, job, kind, payload.clone(), self.now(), limit)
            .await;
        if let (Ok(Some(seq)), Some(&conn)) = (pushed, self.hubs.get(client)) {
            if let Some(frame) = with_seq(&payload, seq) {
                self.send_frame(conn, frame);
            }
        }
    }

    pub(crate) async fn finish(
        &mut self,
        key: &JobKey,
        state: JobState,
        exit_code: Option<i32>,
        reason: Option<ReasonCode>,
        detail: Option<String>,
    ) {
        let msg = BrokerToHub::JobResult(JobResult {
            job: key.job.clone(),
            seq: 0,
            state,
            exit_code,
            reason: reason.clone(),
            detail,
        });
        self.notify_hub(&key.client, &key.job, OutboxKind::JobResult, &msg)
            .await;
        self.publish(
            Some(&key.client),
            QueueEvent::JobFinished {
                job: key.job.clone(),
                state,
                reason,
            },
        );
    }

    fn ack_runner(&self, conn: ConnId, job: &JobId, seq: u64) {
        self.send(
            conn,
            &BrokerToRunner::Ack(Ack {
                job: job.clone(),
                seq,
            }),
        );
    }

    async fn runner_seq(
        &mut self,
        conn: ConnId,
        runner: &str,
        job: &JobId,
        seq: u64,
    ) -> Option<usize> {
        let found = self
            .active
            .iter()
            .position(|j| j.runner.as_deref() == Some(runner) && &j.key.job == job);
        let Some(i) = found else {
            self.ack_runner(conn, job, seq);
            return None;
        };
        let key = self.active[i].key.clone();
        match self.store.accept_seq(&key, Inbound::Runner, seq).await {
            Ok(true) => Some(i),
            Ok(false) => {
                self.ack_runner(conn, job, seq);
                None
            }
            Err(StoreError::SeqGap { expected, got }) => {
                self.send(
                    conn,
                    &BrokerToRunner::Error(ProtocolError {
                        code: ProtocolErrorCode::SeqGap,
                        message: format!("job {job}: expected seq {expected}, got {got}"),
                        supported: None,
                    }),
                );
                None
            }
            Err(_) => None,
        }
    }

    pub(crate) async fn on_runner(&mut self, conn: ConnId, name: String, frame: Frame) {
        let Some(kind) = self.conns.get(&conn).map(|c| peer_kind(&c.peer.endpoint)) else {
            return;
        };
        let msg = match receive::<RunnerToBroker>(frame.as_bytes(), kind) {
            Ok(Received::Message(m)) => m,
            Ok(Received::Ignored(_)) => return,
            Err(e) => {
                if let Some(r) = e.refusal() {
                    self.send(conn, &BrokerToRunner::Error(r));
                }
                return;
            }
        };
        let now = self.now();
        match msg {
            RunnerToBroker::Heartbeat => self.runner_heartbeat(&name).await,
            RunnerToBroker::Bye(_) => {
                self.runner_gone(&name).await;
                self.close(conn);
            }
            RunnerToBroker::Started(s) => {
                let Some(i) = self.runner_seq(conn, &name, &s.job, s.seq).await else {
                    return;
                };
                self.started(i, &name, now).await;
                self.ack_runner(conn, &s.job, s.seq);
            }
            RunnerToBroker::Chunk(c) => {
                let Some(i) = self.runner_seq(conn, &name, &c.job, c.seq).await else {
                    return;
                };
                let key = self.active[i].key.clone();
                let limit = self.log_limit();
                let _ = self
                    .store
                    .append_log(&key, c.text.as_bytes(), now, limit)
                    .await;
                let msg = BrokerToHub::Chunk(Chunk {
                    seq: 0,
                    ..c.clone()
                });
                self.notify_hub(&key.client, &key.job, OutboxKind::Chunk, &msg)
                    .await;
                self.ack_runner(conn, &c.job, c.seq);
            }
            RunnerToBroker::StepResult(s) => {
                let Some(i) = self.runner_seq(conn, &name, &s.job, s.seq).await else {
                    return;
                };
                self.step_result(i, &s, now).await;
                self.ack_runner(conn, &s.job, s.seq);
            }
            RunnerToBroker::JobResult(r) => {
                let Some(i) = self.runner_seq(conn, &name, &r.job, r.seq).await else {
                    return;
                };
                let job = self.active.remove(i);
                self.job_result(job, &r, now).await;
                self.ack_runner(conn, &r.job, r.seq);
                self.plan().await;
            }
            RunnerToBroker::Hello(_) | RunnerToBroker::Join(_) => self.send(
                conn,
                &BrokerToRunner::Error(ProtocolError {
                    code: ProtocolErrorCode::Malformed,
                    message: "already registered on this connection".into(),
                    supported: None,
                }),
            ),
            _ => {}
        }
    }

    async fn started(&mut self, i: usize, runner: &str, now: Timestamp) {
        let key = self.active[i].key.clone();
        let changed = self
            .store
            .transition(&key, Transition::to(JobState::Running, now))
            .await;
        if !matches!(changed, Ok(Applied::Changed)) {
            return;
        }
        self.active[i].started_at = Some(now);
        if let Some(c) = self.active[i].cancel.clone() {
            let mut t = Transition::to(JobState::Cancelling, now);
            t.cancelled_by = Some(c.by);
            t.cancel_reason = Some(c.reason);
            let _ = self.store.transition(&key, t).await;
        }
        let msg = BrokerToHub::Started(StartedNotice {
            job: key.job.clone(),
            seq: 0,
            runner: runner.to_string(),
            started_at: now,
        });
        self.notify_hub(&key.client, &key.job, OutboxKind::Started, &msg)
            .await;
        self.publish(
            Some(&key.client),
            QueueEvent::JobStarted {
                job: key.job.clone(),
                runner: runner.to_string(),
            },
        );
    }

    async fn step_result(&mut self, i: usize, s: &StepResult, now: Timestamp) {
        let key = self.active[i].key.clone();
        let timeout_s = self.active[i]
            .spec
            .steps
            .get(s.ord as usize)
            .map_or(0, |st| st.timeout_s);
        let update = StepUpdate {
            ord: s.ord,
            state: s.state,
            exit_code: s.exit_code,
            started_at: None,
            finished_at: Some(now),
        };
        if !matches!(
            self.store.update_step(&key, update).await,
            Ok(Applied::Changed)
        ) {
            return;
        }
        let msg = BrokerToHub::StepResult(StepResult {
            seq: 0,
            ..s.clone()
        });
        self.notify_hub(&key.client, &key.job, OutboxKind::StepResult, &msg)
            .await;
        self.publish(
            Some(&key.client),
            QueueEvent::StepChanged {
                job: key.job.clone(),
                step: StepView {
                    ord: s.ord,
                    name: s.name.clone(),
                    state: s.state,
                    exit_code: s.exit_code,
                    started_at: None,
                    finished_at: Some(now),
                    timeout_s,
                },
            },
        );
    }

    async fn job_result(&mut self, job: Job, r: &JobResult, now: Timestamp) {
        if job.started_at.is_none() && r.state != JobState::Cancelled {
            let _ = self
                .store
                .transition(&job.key, Transition::to(JobState::Running, now))
                .await;
        }
        let reason = r
            .reason
            .clone()
            .or_else(|| job.cancel.as_ref().map(|c| c.code.clone()));
        let mut t = Transition::to(r.state, now);
        t.exit_code = r.exit_code;
        match &job.cancel {
            Some(c) => {
                t.cancelled_by = Some(c.by.clone());
                t.cancel_reason = Some(c.reason.clone());
            }
            None => {
                t.cancel_reason = r.reason.as_ref().map(|code| match &r.detail {
                    Some(d) => format!("{code}: {d}"),
                    None => code.to_string(),
                });
            }
        }
        if let Ok(Applied::Changed) = self.store.transition(&job.key, t).await {
            self.finish(&job.key, r.state, r.exit_code, reason, r.detail.clone())
                .await;
        }
    }
}
