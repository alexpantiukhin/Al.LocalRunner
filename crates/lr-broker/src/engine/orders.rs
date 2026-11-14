use super::assign::with_seq;
use super::conn::ConnId;
use super::hello::peer_kind;
use super::{Client, Engine, Job};
use crate::policy::TimeoutAboveMax;
use lr_abstractions::{
    CodeSource, GithubRunRow, Inbound, Intake, JobKey, JobRecord, JobRow, JobSpecRow, Level,
    OrderRow, SourceKind, StepRow, StoreError, Transition,
};
use lr_core::{
    ClientView, Declared, JobState, MachineRef, PoolLimits, QueueEntry, Reject, Requirements,
    Source, StepState,
};
use lr_proto::{
    receive, Accepted, BrokerToHub, HubToBroker, Order, OrderSpec, ProtocolErrorCode, QueueEvent,
    ReadAnswer, ReadReply, ReadRequest, ReasonCode, Received, RejectReason, Rejected,
    ResourceOverride, Resume, WireCode,
};

pub(super) fn reject_reason(r: &Reject) -> RejectReason {
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

fn rejected(o: &Order, reason: RejectReason, detail: String) -> BrokerToHub {
    BrokerToHub::Rejected(Rejected {
        job: o.job.clone(),
        order_seq: o.seq,
        code: ReasonCode::Rejected(reason),
        detail,
    })
}

/// The refusal kept for a run that `record_rejected` stored, so an order sent again after the
/// answer got lost is refused again instead of being taken for an accepted one.
fn recorded_rejection(rec: &JobRecord) -> Option<(RejectReason, String)> {
    if rec.job.state != JobState::Cancelled {
        return None;
    }
    let text = rec.job.cancel_reason.as_deref()?;
    let Some(ReasonCode::Rejected(reason)) = crate::api::reason_of(Some(text)) else {
        return None;
    };
    let detail = text.split_once(": ").map_or("", |(_, d)| d).to_string();
    Some((reason, detail))
}

pub(super) fn overridden(
    r: &Reject,
    over: &ResourceOverride,
    req: &Requirements,
    pool: &PoolLimits,
) -> Option<(RejectReason, String)> {
    let caused = match r {
        Reject::PoolMemory { .. } => over.mem_mb.is_some(),
        Reject::PoolCpu { .. } => over.cpu.is_some(),
        _ => false,
    };
    if !caused {
        return None;
    }
    let mut parts = Vec::new();
    if over.mem_mb.is_some() && req.mem_mb > pool.mem_mb {
        parts.push(format!(
            "--mem {} МБ больше пула: максимум в пуле {} МБ",
            req.mem_mb, pool.mem_mb
        ));
    }
    if over.cpu.is_some() && req.cpu > pool.cpu {
        parts.push(format!(
            "--cpu {} больше пула: максимум в пуле {} CPU",
            req.cpu, pool.cpu
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some((RejectReason::OverrideAbovePool, parts.join("; ")))
}

fn intake(key: &JobKey, client: &Client, spec: &OrderSpec, entry: &QueueEntry) -> Intake {
    let req = &entry.order.requirements;
    let id = &spec.identity;
    let (code_source, commit) = match &spec.code {
        WireCode::WorkTree { .. } => (CodeSource::WorkTree, None),
        WireCode::Commit { sha, .. } => (CodeSource::Commit, Some(sha.clone())),
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
            peak_mem_mb: None,
            peak_disk_mb: None,
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
        self.client_seen.insert(client.key.clone(), self.now());
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
        if let Some(detail) = Self::counted(&msg)
            .then(|| self.rate_limited(&client.key))
            .flatten()
        {
            return self.refuse_rate(conn, msg, detail);
        }
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

    fn counted(msg: &HubToBroker) -> bool {
        match msg {
            HubToBroker::Read(call) => !matches!(
                call.request,
                ReadRequest::Unsubscribe { .. } | ReadRequest::Unfollow { .. }
            ),
            _ => false,
        }
    }

    fn refuse_rate(&mut self, conn: ConnId, msg: HubToBroker, detail: String) {
        let code = ReasonCode::Rejected(RejectReason::RateLimited);
        self.diag(
            Level::Info,
            "request refused",
            &[("conn", &conn.to_string()), ("reason", &code.to_string())],
        );
        let reply = match msg {
            HubToBroker::Order(o) => BrokerToHub::Rejected(Rejected {
                job: o.job,
                order_seq: o.seq,
                code,
                detail,
            }),
            HubToBroker::Read(call) => BrokerToHub::ReadReply(ReadAnswer {
                req: call.req,
                reply: ReadReply::Denied {
                    reason: format!("{code}: {detail}"),
                },
            }),
            _ => return,
        };
        self.send(conn, &reply);
    }

    pub(crate) fn unavailable(&self, conn: ConnId, key: &JobKey, what: &str, e: &StoreError) {
        let error = e.to_string();
        self.diag(
            Level::Error,
            &format!("storage failed: could not {what}"),
            &[
                ("client", &key.client),
                ("job", &key.job.0),
                ("error", &error),
            ],
        );
        self.error(
            conn,
            ProtocolErrorCode::Unavailable,
            format!(
                "broker storage failed, could not {what} for {}; retry later",
                key.job
            ),
        );
    }

    fn admit(&self, entry: &QueueEntry, spec: &OrderSpec) -> Result<(), (RejectReason, String)> {
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
        self.allowed(entry, spec)?;
        self.admit_fit(entry, spec)
    }

    fn job_of(&mut self, client: &Client, key: &JobKey, mut spec: OrderSpec) -> Job {
        spec.requirements.commands = self.command_names(&client.key, &client.label, &spec.steps);
        let mut requirements = spec.requirements.clone();
        if requirements.same_machine.is_some() {
            requirements.same_machine = Some(MachineRef(client.key.clone()));
        }
        let entry = QueueEntry {
            order: lr_core::Order {
                job: self.internal_id(),
                requirements,
                enqueued_at: spec.enqueued_at,
                declared: Declared::from_spec(&spec.identity, &spec.labels),
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
        Job {
            key: key.clone(),
            internal: entry.order.job.clone(),
            entry,
            spec,
            runner: None,
            assigned_at: self.now(),
            started_at: None,
            cancel: None,
            start_unsaved: false,
            queued_since: self.now(),
            platform_label: None,
            confirmed: false,
            gone_since: None,
            lost_floor: None,
        }
    }

    async fn enqueue(&mut self, job: Job) {
        let summary = crate::api::summary_of_job(&job);
        let client = job.key.client.clone();
        self.queue.push(job);
        self.publish(
            Some(&client),
            QueueEvent::JobQueued {
                job: Box::new(summary),
            },
        );
        self.plan().await;
    }

    fn accepted(&self, key: &JobKey, o: &Order, trimmed: Option<u64>) -> BrokerToHub {
        BrokerToHub::Accepted(Accepted {
            job: o.job.clone(),
            order_seq: o.seq,
            waiting: self.waits.get(key).cloned(),
            timeout_trimmed_to: trimmed,
        })
    }

    fn trim_timeout(&self, spec: &mut OrderSpec) -> Option<u64> {
        let limits = &self.policy.file().limits;
        let max = limits.max_job_timeout_s?.get();
        if limits.timeout_above_max != TimeoutAboveMax::Trim || spec.requirements.timeout_s <= max {
            return None;
        }
        spec.requirements.timeout_s = max;
        for step in &mut spec.steps {
            step.timeout_s = step.timeout_s.min(max);
        }
        Some(max)
    }

    /// Keeps a refused order in the history as a cancelled run whose reason is the rejection
    /// code and the explanation, so `history`, `stats` and the dashboard can answer what was
    /// asked for and why it was refused. A storage failure only costs the record.
    async fn record_rejected(
        &mut self,
        client: &Client,
        job: &Job,
        reason: &RejectReason,
        detail: &str,
    ) {
        let code = ReasonCode::Rejected(reason.clone());
        let stored = intake(&job.key, client, &job.spec, &job.entry);
        let recorded = match self.store.accept(stored).await {
            Ok(_) => {
                let mut end = Transition::to(JobState::Cancelled, self.now());
                end.cancelled_by = Some("broker".into());
                end.cancel_reason = Some(format!("{code}: {detail}"));
                self.store.transition(&job.key, end).await.map(|_| ())
            }
            Err(e) => Err(e),
        };
        if let Err(e) = recorded {
            self.diag(
                Level::Warn,
                "storage failed: the rejected order is not in the history",
                &[
                    ("client", &job.key.client),
                    ("job", &job.key.job.0),
                    ("error", &e.to_string()),
                ],
            );
        }
    }

    async fn order(&mut self, conn: ConnId, client: &Client, mut o: Order) {
        let trimmed = self.trim_timeout(&mut o.spec);
        o.spec.enqueued_at = self.now();
        let key = JobKey {
            client: client.key.clone(),
            job: o.job.clone(),
        };
        match self.store.job(&key).await {
            Ok(Some(rec)) => {
                return self
                    .repeated_order(conn, client, &key, rec, o, trimmed)
                    .await
            }
            Ok(None) => {}
            Err(e) => return self.unavailable(conn, &key, "read the order", &e),
        }
        if let Some(detail) = self.rate_limited(&client.key) {
            return self.refuse_rate(conn, HubToBroker::Order(o), detail);
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
        let job = self.job_of(client, &key, *o.spec.clone());
        if let Err((reason, detail)) = self.admit(&job.entry, &job.spec) {
            self.count_rejected(&job);
            self.note_rejected(&job, &reason);
            self.record_rejected(client, &job, &reason, &detail).await;
            return self.send(conn, &rejected(&o, reason, detail));
        }
        let intake = intake(&key, client, &job.spec, &job.entry);
        if let Err(e) = self.store.accept(intake).await {
            return self.unavailable(conn, &key, "store the order", &e);
        }
        if let Err(e) = self.store.accept_seq(&key, Inbound::Hub, o.seq).await {
            return self.unavailable(conn, &key, "record the order seq", &e);
        }
        self.note_accepted(&job);
        self.enqueue(job).await;
        self.send(conn, &self.accepted(&key, &o, trimmed));
    }

    async fn repeated_order(
        &mut self,
        conn: ConnId,
        client: &Client,
        key: &JobKey,
        rec: JobRecord,
        o: Order,
        trimmed: Option<u64>,
    ) {
        let last = match self.store.last_seq(key, Inbound::Hub).await {
            Ok(last) => last,
            Err(e) => return self.unavailable(conn, key, "read the order seq", &e),
        };
        if o.seq == 0 || o.seq > last.max(1) {
            return self.error(
                conn,
                ProtocolErrorCode::Malformed,
                format!(
                    "order {} is already accepted; a repeated order keeps seq 1, got {}",
                    o.job, o.seq
                ),
            );
        }
        if last == 0 {
            if let Some((reason, detail)) = recorded_rejection(&rec) {
                return self.send(conn, &rejected(&o, reason, detail));
            }
            if let Err(e) = self.store.accept_seq(key, Inbound::Hub, o.seq).await {
                return self.unavailable(conn, key, "record the order seq", &e);
            }
        }
        let parked = self.parked_keys().any(|k| k == key);
        if rec.job.state == JobState::Queued && self.find(key).is_none() && !parked {
            let job = match self.stored_job(&rec) {
                Some(mut job) => {
                    job.runner = None;
                    job.entry.client.endpoint = client.endpoint.clone();
                    job
                }
                None => {
                    let mut spec = *o.spec.clone();
                    spec.enqueued_at = rec.job.queued_at;
                    let job = self.job_of(client, key, spec);
                    if let Err((reason, detail)) = self.admit(&job.entry, &job.spec) {
                        self.count_rejected(&job);
                        self.note_rejected(&job, &reason);
                        let mut end = Transition::to(JobState::Cancelled, self.now());
                        end.cancel_reason = Some(ReasonCode::Rejected(reason.clone()).to_string());
                        if let Err(e) = self.store.transition(key, end).await {
                            return self.unavailable(conn, key, "end the unreadable order", &e);
                        }
                        return self.send(conn, &rejected(&o, reason, detail));
                    }
                    let restored = intake(key, client, &job.spec, &job.entry).spec;
                    if let Err(e) = self.store.replace_spec(key, restored).await {
                        return self.unavailable(conn, key, "restore the order spec", &e);
                    }
                    job
                }
            };
            self.enqueue(job).await;
        }
        self.send(conn, &self.accepted(key, &o, trimmed));
    }

    async fn resume(&mut self, conn: ConnId, client: &Client, r: Resume) {
        let Ok(pending) = self.store.pending_results(&client.key).await else {
            return;
        };
        let mut acked = Vec::new();
        let mut frames = Vec::new();
        for entry in pending {
            let last = r.last_seq(&entry.job);
            if entry.seq <= last {
                if !acked.contains(&entry.job) {
                    let _ = self.store.ack_results(&client.key, &entry.job, last).await;
                    acked.push(entry.job.clone());
                }
                continue;
            }
            frames.extend(with_seq(&entry.payload, entry.seq));
        }
        self.send_ordered(conn, frames);
    }
}
