use super::assign::with_seq;
use super::conn::ConnId;
use super::hello::peer_kind;
use super::{Cancel, Client, Engine, Job};
use crate::policy::CancelOthers;
use lr_abstractions::{
    CodeSource, GithubRunRow, Inbound, Intake, JobKey, JobRow, JobSpecRow, OrderRow, OutboxKind,
    SourceKind, StepRow, StoreError, Transition,
};
use lr_core::{
    check_runners, ClientView, Declared, JobState, MachineRef, QueueEntry, Reject, Source,
    StepState,
};
use lr_proto::{
    receive, Accepted, Ack, BrokerToHub, BrokerToRunner, CancelAck, CancelOutcome, HubCancel,
    HubToBroker, Order, OrderSpec, ProtocolErrorCode, QueueEvent, ReasonCode, Received,
    RejectReason, Rejected, Resume, RunnerCancel, WireCode,
};

fn reject_reason(r: &Reject) -> RejectReason {
    match r {
        Reject::PoolMemory { .. } => RejectReason::PoolMemory,
        Reject::PoolCpu { .. } => RejectReason::PoolCpu,
        Reject::Timeout { .. } => RejectReason::Timeout,
        Reject::Reserved { .. } => RejectReason::Reserved,
        Reject::Quota { .. } => RejectReason::Quota,
        Reject::NoMatchingRunner { .. } => RejectReason::NoMatchingRunner,
        Reject::NoRunnerOnMachine => RejectReason::NoRunnerOnMachine,
    }
}

fn intake(key: &JobKey, client: &Client, spec: &OrderSpec, entry: &QueueEntry) -> Intake {
    let req = &entry.order.requirements;
    let id = &spec.identity;
    let (code_source, commit) = match &spec.code {
        WireCode::WorkTree { .. } => (CodeSource::WorkTree, None),
        WireCode::Commit { sha } => (CodeSource::Commit, Some(sha.clone())),
    };
    let (source, github) = match &id.source {
        Source::Session => (SourceKind::Session, None),
        Source::Github(g) => (
            SourceKind::Github,
            Some(GithubRunRow {
                repository: g.repository.clone(),
                workflow: g.workflow.clone(),
                job_name: g.job.clone(),
                run_id: g.run_id,
                attempt: g.attempt,
                git_ref: g.git_ref.clone(),
                actor: g.actor.clone(),
                run_url: g.run_url.clone(),
            }),
        ),
    };
    let json = serde_json::to_string(spec).unwrap_or_default();
    Intake {
        job: JobRow {
            key: key.clone(),
            project: id.project.clone(),
            source,
            label: id.label.clone(),
            note: id.note.clone(),
            branch: id.branch.clone(),
            cwd: id.cwd.display().to_string(),
            profile: spec.profile.clone(),
            platform: req.platform.clone(),
            runner: None,
            commit_sha: commit.clone(),
            client_machine: client.machine_id.clone(),
            client_label: client.label.clone(),
            os_user: id.os_user.clone(),
            secrets: req.secrets.iter().cloned().collect(),
            code_source,
            queued_at: spec.enqueued_at,
            started_at: None,
            finished_at: None,
            state: JobState::Queued,
            exit_code: None,
            cancelled_by: None,
            cancel_reason: None,
        },
        steps: spec
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| StepRow {
                ord: i as u32,
                name: s.name.clone(),
                command: s.command.clone(),
                state: StepState::Queued,
                exit_code: None,
                started_at: None,
                finished_at: None,
                timeout_s: s.timeout_s,
            })
            .collect(),
        spec: JobSpecRow {
            resolved_json: json.clone(),
            profile_commit: commit,
        },
        github,
        order: OrderRow {
            key: key.clone(),
            client_machine: client.machine_id.clone(),
            spec_json: json,
            platform: req.platform.clone(),
            same_machine: req.same_machine.is_some(),
            priority_hint: spec.wanted_priority,
            enqueued_at: spec.enqueued_at,
            state: "queued".into(),
        },
    }
}

impl Engine {
    pub(crate) async fn on_hub(
        &mut self,
        conn: ConnId,
        client: Client,
        frame: lr_abstractions::Frame,
    ) {
        let msg = match receive::<HubToBroker>(frame.as_bytes(), peer_kind(&client.endpoint)) {
            Ok(Received::Message(m)) => m,
            Ok(Received::Ignored(_)) => return,
            Err(e) => {
                if let Some(r) = e.refusal() {
                    self.send(conn, &BrokerToHub::Error(r));
                }
                return;
            }
        };
        match msg {
            HubToBroker::Order(o) => self.order(conn, &client, o).await,
            HubToBroker::Cancel(c) => self.cancel(conn, &client, c).await,
            HubToBroker::Resume(r) => self.resume(conn, &client, r).await,
            HubToBroker::Ack(a) => {
                let _ = self.store.ack_results(&client.key, &a.job, a.seq).await;
            }
            HubToBroker::Heartbeat => {
                if let Some(name) = &client.machine {
                    let _ = self.store.touch_machine(name, self.now()).await;
                }
            }
            HubToBroker::Bye(_) => self.close(conn),
            HubToBroker::Read(call) => self.read(conn, &client, call).await,
            HubToBroker::CapabilitiesQuery(q) => self.capabilities(conn, &client, q),
            HubToBroker::Hello(_) | HubToBroker::Join(_) => self.error(
                conn,
                ProtocolErrorCode::Malformed,
                "already registered on this connection".into(),
            ),
            HubToBroker::Error(_) => {}
        }
    }

    fn admit(&self, entry: &QueueEntry) -> Result<(), (RejectReason, String)> {
        let limit = self.policy.file().limits.max_active_per_client.get() as usize;
        let active = self
            .queue
            .iter()
            .chain(self.active.iter())
            .filter(|j| j.key.client == entry.client.key)
            .count();
        if active >= limit {
            return Err((
                RejectReason::QueueLimit,
                format!("активных заказов у клиента {active}, предел {limit}"),
            ));
        }
        let fail = |r: Reject| (reject_reason(&r), r.to_string());
        self.policy
            .policy()
            .admit(&entry.order, &entry.client)
            .map_err(fail)?;
        check_runners(entry, &self.runner_views(), &self.site).map_err(fail)
    }

    async fn order(&mut self, conn: ConnId, client: &Client, o: Order) {
        let key = JobKey {
            client: client.key.clone(),
            job: o.job.clone(),
        };
        let accepted = |waiting| {
            BrokerToHub::Accepted(Accepted {
                job: o.job.clone(),
                order_seq: o.seq,
                waiting,
            })
        };
        if let Ok(Some(_)) = self.store.job(&key).await {
            let _ = self.store.accept_seq(&key, Inbound::Hub, o.seq).await;
            return self.send(conn, &accepted(self.waits.get(&key).cloned()));
        }
        if o.seq != 1 {
            return self.error(
                conn,
                ProtocolErrorCode::SeqGap,
                format!(
                    "order {} must open its stream at seq 1, got {}",
                    o.job, o.seq
                ),
            );
        }
        let spec = *o.spec.clone();
        let mut requirements = spec.requirements.clone();
        if requirements.same_machine.is_some() {
            requirements.same_machine = Some(MachineRef(client.key.clone()));
        }
        let entry = QueueEntry {
            order: lr_core::Order {
                job: self.internal_id(),
                requirements,
                enqueued_at: spec.enqueued_at,
                declared: Declared::from_identity(&spec.identity),
                wanted_priority: spec.wanted_priority,
            },
            client: ClientView {
                key: client.key.clone(),
                label: client.label.clone(),
                machine_id: client.machine_id.clone(),
                endpoint: client.endpoint.clone(),
            },
            accepted_at: self.now(),
        };
        if let Err((reason, detail)) = self.admit(&entry) {
            return self.send(
                conn,
                &BrokerToHub::Rejected(Rejected {
                    job: o.job.clone(),
                    order_seq: o.seq,
                    code: ReasonCode::Rejected(reason),
                    detail,
                }),
            );
        }
        if self
            .store
            .accept(intake(&key, client, &spec, &entry))
            .await
            .is_err()
        {
            return;
        }
        let _ = self.store.accept_seq(&key, Inbound::Hub, o.seq).await;
        let job = Job {
            key: key.clone(),
            internal: entry.order.job.clone(),
            entry,
            spec,
            runner: None,
            assigned_at: self.now(),
            started_at: None,
            cancel: None,
        };
        let summary = crate::api::summary_of_job(&job);
        self.queue.push(job);
        self.publish(Some(&key.client), QueueEvent::JobQueued { job: summary });
        self.plan().await;
        self.send(conn, &accepted(self.waits.get(&key).cloned()));
    }

    async fn cancel(&mut self, conn: ConnId, client: &Client, c: HubCancel) {
        let own = JobKey {
            client: client.key.clone(),
            job: c.job.clone(),
        };
        let ack = BrokerToHub::Ack(Ack {
            job: c.job.clone(),
            seq: c.seq,
        });
        if let Ok(Some(rec)) = self.store.job(&own).await {
            match self.store.accept_seq(&own, Inbound::Hub, c.seq).await {
                Ok(true) => {}
                Ok(false) => return self.send(conn, &ack),
                Err(StoreError::SeqGap { expected, got }) => {
                    return self.error(
                        conn,
                        ProtocolErrorCode::SeqGap,
                        format!("job {}: expected seq {expected}, got {got}", c.job),
                    )
                }
                Err(_) => return,
            }
            self.send(conn, &ack);
            let outcome = if rec.job.state.is_final() {
                CancelOutcome::AlreadyFinal
            } else {
                self.apply_cancel(&own, &c).await;
                CancelOutcome::Applied
            };
            return self.cancel_ack(&client.key, &c, outcome, None).await;
        }
        self.send(conn, &ack);
        let foreign = self
            .queue
            .iter()
            .chain(self.active.iter())
            .find(|j| j.key.job == c.job)
            .map(|j| j.key.clone());
        let others = self.policy.file().cancel.others;
        match foreign {
            Some(key) if client.owner && others == CancelOthers::Owner => {
                self.apply_cancel(&key, &c).await;
                self.cancel_ack(&client.key, &c, CancelOutcome::Applied, None)
                    .await;
            }
            Some(_) => {
                let why = "чужой заказ: отменять чужие заказы может только владелец брокера \
                           ([cancel] others)";
                self.cancel_ack(&client.key, &c, CancelOutcome::Denied, Some(why.into()))
                    .await;
            }
            None => {
                let why = format!("заказ {} не найден", c.job);
                self.cancel_ack(&client.key, &c, CancelOutcome::Denied, Some(why))
                    .await;
            }
        }
    }

    async fn cancel_ack(
        &mut self,
        client: &str,
        c: &HubCancel,
        outcome: CancelOutcome,
        denial: Option<String>,
    ) {
        let msg = BrokerToHub::CancelAck(CancelAck {
            job: c.job.clone(),
            seq: 0,
            cancel_seq: c.seq,
            outcome,
            denial,
        });
        self.notify_hub(client, &c.job, OutboxKind::CancelAck, &msg)
            .await;
    }

    async fn apply_cancel(&mut self, key: &JobKey, c: &HubCancel) {
        let cancel = Cancel {
            by: c.by.label.clone(),
            reason: match &c.reason {
                Some(r) => format!("{}: {r}", c.code),
                None => c.code.to_string(),
            },
            code: c.code.clone(),
            detail: c.reason.clone(),
        };
        if let Some(i) = self.queue.iter().position(|j| &j.key == key) {
            let job = self.queue.remove(i);
            self.cancel_waiting(job, cancel).await;
            return self.plan().await;
        }
        let Some(i) = self.active.iter().position(|j| &j.key == key) else {
            return;
        };
        if self.active[i].cancel.is_some() {
            return;
        }
        self.active[i].cancel = Some(cancel.clone());
        let now = self.now();
        if self.active[i].started_at.is_some() {
            let mut t = Transition::to(JobState::Cancelling, now);
            t.cancelled_by = Some(cancel.by.clone());
            t.cancel_reason = Some(cancel.reason.clone());
            let _ = self.store.transition(key, t).await;
        }
        let runner = self.active[i].runner.clone().unwrap_or_default();
        let seq = self.store.next_seq_to_runner(key).await.unwrap_or(1);
        if let Some(conn) = self.runner_conn(&runner) {
            self.send(
                conn,
                &BrokerToRunner::Cancel(RunnerCancel {
                    job: key.job.clone(),
                    seq,
                    code: cancel.code,
                    detail: cancel.detail,
                }),
            );
        }
    }

    pub(crate) async fn cancel_waiting(&mut self, job: Job, cancel: Cancel) {
        let mut t = Transition::to(JobState::Cancelled, self.now());
        t.cancelled_by = Some(cancel.by);
        t.cancel_reason = Some(cancel.reason);
        if self.store.transition(&job.key, t).await.is_ok() {
            self.waits.remove(&job.key);
            self.finish(
                &job.key,
                JobState::Cancelled,
                None,
                Some(cancel.code),
                cancel.detail,
            )
            .await;
        }
    }

    async fn resume(&mut self, conn: ConnId, client: &Client, r: Resume) {
        let Ok(pending) = self.store.pending_results(&client.key).await else {
            return;
        };
        let mut acked = Vec::new();
        for entry in pending {
            let last = r.last_seq(&entry.job);
            if entry.seq <= last {
                if !acked.contains(&entry.job) {
                    let _ = self.store.ack_results(&client.key, &entry.job, last).await;
                    acked.push(entry.job.clone());
                }
                continue;
            }
            if let Some(frame) = with_seq(&entry.payload, entry.seq) {
                self.send_frame(conn, frame);
            }
        }
    }
}
