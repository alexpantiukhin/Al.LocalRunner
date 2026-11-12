use super::conn::ConnId;
use super::hello::peer_kind;
use super::retry::Retry;
use super::{Engine, Job};
use crate::policy::CacheShare;
use lr_abstractions::{Applied, Frame, Inbound, JobKey, Level, OutboxKind, StepUpdate, StoreError};
use lr_core::{Assignment, JobId, JobState, RunningJob, Timestamp, Wait};
use lr_proto::{
    receive, Ack, Assign, AssignSpec, BrokerToHub, BrokerToRunner, Chunk, JobResult, ProtocolError,
    ProtocolErrorCode, QueueEvent, ReasonCode, Received, RunnerToBroker, StepResult, StepView,
    SHARED_CACHE_CLIENT,
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
        let plan = self.plan_pass(now);
        self.waits = plan
            .waiting
            .iter()
            .filter_map(|(id, wait)| {
                let key = self.queue.iter().find(|j| &j.internal == id)?.key.clone();
                Some((key, self.wait_text(wait)))
            })
            .collect();
        self.wait_codes = plan
            .waiting
            .iter()
            .filter_map(|(id, wait)| {
                let key = self.queue.iter().find(|j| &j.internal == id)?.key.clone();
                Some((key, super::metrics::wait_code(wait)))
            })
            .collect();
        self.evict_idle(plan.waiting.first().map(|(_, w)| w));
        self.note_waits(&plan.waiting);
        self.note_rules(&plan.waiting);
        for a in plan.assignments {
            self.assign(a, now).await;
        }
        self.report();
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
        let seq = match self.store.next_seq_to_runner(&job.key).await {
            Ok(seq) => seq,
            Err(e) => {
                self.store_failed(
                    "number the assignment; the run goes back to the queue",
                    &job.key,
                    &e,
                );
                job.runner = Some(a.runner);
                return self.requeue_or_retry(job).await;
            }
        };
        let spec = &job.spec;
        let cache_client = (self.policy.file().cache.share == CacheShare::Project)
            .then(|| SHARED_CACHE_CLIENT.to_string());
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
                    named_commands: !job.entry.order.requirements.commands.is_empty(),
                    cache_client,
                    limits: self.policy.file().resources.as_ref().map(|r| r.wire()),
                }),
            }),
        );
        job.platform_label = Some(self.platform_label(&job));
        self.note_assigned(&job, &a.runner);
        self.observe_wait(&job, now);
        let group = self.policy.policy().group_of(&job.entry.client);
        self.served.record(&group, now);
        job.runner = Some(a.runner);
        job.assigned_at = now;
        self.waits.remove(&job.key);
        self.wait_codes.remove(&job.key);
        self.active.push(job);
    }

    pub(crate) async fn notify_hub(
        &mut self,
        client: &str,
        job: &JobId,
        kind: OutboxKind,
        msg: &BrokerToHub,
    ) {
        if !self.delivers_to(client, job) {
            return;
        }
        let Ok(payload) = serde_json::to_string(msg) else {
            return;
        };
        let key = JobKey {
            client: client.to_string(),
            job: job.clone(),
        };
        self.push_hub(&key, kind, payload).await;
    }

    pub(crate) async fn push_hub(&mut self, key: &JobKey, kind: OutboxKind, payload: String) {
        let limit = self.outbox_limit();
        let pushed = self
            .store
            .push_result(
                &key.client,
                &key.job,
                kind,
                payload.clone(),
                self.now(),
                limit,
            )
            .await;
        match pushed {
            Ok(Some(seq)) => {
                if let Some(&conn) = self.hubs.get(&key.client) {
                    if let Some(frame) = with_seq(&payload, seq) {
                        self.send_ordered(conn, [frame]);
                    }
                }
            }
            Ok(None) => {}
            Err(e) if kind == OutboxKind::Chunk => self.store_failed(
                "keep a log chunk for the hub; the chunk is dropped",
                key,
                &e,
            ),
            Err(e) => self.retry_later(
                "keep a message for the hub",
                &e,
                Retry::Notify(key.clone(), kind, payload),
            ),
        }
    }

    pub(crate) async fn finish(
        &mut self,
        job: &Job,
        state: JobState,
        exit_code: Option<i32>,
        reason: Option<ReasonCode>,
        detail: Option<String>,
    ) {
        self.finish_with_peaks(job, state, exit_code, reason, detail, (None, None))
            .await
    }

    pub(crate) async fn finish_with_peaks(
        &mut self,
        job: &Job,
        state: JobState,
        exit_code: Option<i32>,
        reason: Option<ReasonCode>,
        detail: Option<String>,
        peaks: (Option<u64>, Option<u64>),
    ) {
        self.count_end(job, state);
        let key = &job.key;
        let msg = BrokerToHub::JobResult(JobResult {
            job: key.job.clone(),
            seq: 0,
            state,
            exit_code,
            reason: reason.clone(),
            detail,
            peak_mem_mb: peaks.0,
            peak_disk_mb: peaks.1,
        });
        self.notify_hub(&key.client, &key.job, OutboxKind::JobResult, &msg)
            .await;
        self.silenced.remove(key);
        self.end_follows(key);
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
            self.diag(
                Level::Info,
                "late message for a job not assigned to this runner, ignored",
                &[
                    ("runner", runner),
                    ("job", &job.0),
                    ("seq", &seq.to_string()),
                ],
            );
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
            Err(e) => {
                self.store_failed(
                    "record a message seq from the runner; the connection is closed so the runner sends it again",
                    &key,
                    &e,
                );
                self.close(conn);
                None
            }
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
            RunnerToBroker::Heartbeat { load } => self.runner_heartbeat(&name, load).await,
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
                if let Err(e) = self
                    .store
                    .append_log(&key, c.text.as_bytes(), now, limit)
                    .await
                {
                    self.store_failed(
                        "append a log chunk; the chunk is missing from the stored log",
                        &key,
                        &e,
                    );
                }
                self.feed_follows(&key, c.step, &c.text);
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
            RunnerToBroker::Ack(a) => self.confirm_assign(&name, &a.job),
            RunnerToBroker::Resume(r) => self.runner_resumed(&name, &r.points).await,
            RunnerToBroker::Drain => self.drain(&name).await,
            RunnerToBroker::CapabilitiesUpdate(u) => {
                self.capabilities_update(&name, u.capabilities).await
            }
            RunnerToBroker::ServicesReport(r) => self.services_report(&name, r).await,
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

    async fn step_result(&mut self, i: usize, s: &StepResult, now: Timestamp) {
        let key = self.active[i].key.clone();
        let timeout_s = self.active[i]
            .spec
            .steps
            .get(s.ord as usize)
            .map_or(0, |st| st.timeout_s);
        let begun = Timestamp(now.0.saturating_sub(s.duration_ms));
        let not_run = s.state == lr_core::StepState::Skipped
            || (s.duration_ms == 0
                && matches!(
                    s.state,
                    lr_core::StepState::Timeout | lr_core::StepState::Cancelled
                ));
        let started_at =
            (!not_run).then(|| self.active[i].started_at.map_or(begun, |j| begun.max(j)));
        let update = StepUpdate {
            ord: s.ord,
            state: s.state,
            exit_code: s.exit_code,
            started_at,
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
                    started_at,
                    finished_at: Some(now),
                    timeout_s,
                },
            },
        );
    }
}
