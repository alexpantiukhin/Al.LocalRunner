use super::assign::with_seq;
use super::conn::ConnId;
use super::hello::peer_kind;
use super::{Client, Engine, Job};
use lr_abstractions::{
    CodeSource, GithubRunRow, Inbound, Intake, JobKey, JobRow, JobSpecRow, Level, OrderRow,
    SourceKind, StepRow, StoreError,
};
use lr_core::{
    check_runners, ClientView, Declared, JobState, MachineRef, QueueEntry, Reject, Source,
    StepState,
};
use lr_proto::{
    receive, Accepted, BrokerToHub, HubToBroker, Order, OrderSpec, ProtocolErrorCode, QueueEvent,
    ReasonCode, Received, RejectReason, Rejected, Resume, WireCode,
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
        let fail = |r: Reject| (reject_reason(&r), r.to_string());
        self.policy
            .policy()
            .admit(&entry.order, &entry.client)
            .map_err(fail)?;
        check_runners(entry, &self.runner_views(), &self.site).map_err(fail)
    }

    fn job_of(&mut self, client: &Client, key: &JobKey, spec: OrderSpec) -> Job {
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
        Job {
            key: key.clone(),
            internal: entry.order.job.clone(),
            entry,
            spec,
            runner: None,
            assigned_at: self.now(),
            started_at: None,
            cancel: None,
        }
    }

    async fn enqueue(&mut self, job: Job) {
        let summary = crate::api::summary_of_job(&job);
        let client = job.key.client.clone();
        self.queue.push(job);
        self.publish(Some(&client), QueueEvent::JobQueued { job: summary });
        self.plan().await;
    }

    fn accepted(&self, key: &JobKey, o: &Order) -> BrokerToHub {
        BrokerToHub::Accepted(Accepted {
            job: o.job.clone(),
            order_seq: o.seq,
            waiting: self.waits.get(key).cloned(),
        })
    }

    async fn order(&mut self, conn: ConnId, client: &Client, o: Order) {
        let key = JobKey {
            client: client.key.clone(),
            job: o.job.clone(),
        };
        match self.store.job(&key).await {
            Ok(Some(rec)) => {
                return self
                    .repeated_order(conn, client, &key, rec.job.state, o)
                    .await
            }
            Ok(None) => {}
            Err(e) => return self.unavailable(conn, &key, "read the order", &e),
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
        let intake = intake(&key, client, &job.spec, &job.entry);
        if let Err(e) = self.store.accept(intake).await {
            return self.unavailable(conn, &key, "store the order", &e);
        }
        if let Err(e) = self.store.accept_seq(&key, Inbound::Hub, o.seq).await {
            return self.unavailable(conn, &key, "record the order seq", &e);
        }
        self.enqueue(job).await;
        self.send(conn, &self.accepted(&key, &o));
    }

    async fn repeated_order(
        &mut self,
        conn: ConnId,
        client: &Client,
        key: &JobKey,
        state: JobState,
        o: Order,
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
            if let Err(e) = self.store.accept_seq(key, Inbound::Hub, o.seq).await {
                return self.unavailable(conn, key, "record the order seq", &e);
            }
        }
        if state == JobState::Queued && self.find(key).is_none() {
            let job = self.job_of(client, key, *o.spec.clone());
            self.enqueue(job).await;
        }
        self.send(conn, &self.accepted(key, &o));
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
