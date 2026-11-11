use crate::engine::{refusal, CliEvent, ConnId, Engine, LOCAL_BROKER};
use crate::handles::{handle_path, log_path, write_handle};
use crate::limits::Overflow;
use crate::orders::{build, order_payload, Refusal};
use crate::relay::{relay_log, LogRead};
use crate::track::{Stage, Track};
use lr_abstractions::{ActiveOrder, Level, OutboxKind};
use lr_core::{Identity, JobId, JobState};
use lr_proto::{
    BrokerStatus, CancelRequest, CancelTarget, CliRead, CliReply, CliRequest, HubOrder,
    HubToBroker, ProtocolErrorCode, ReadCall, ReadRequest, ReasonCode, RejectReason, SubmitRequest,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

pub(crate) struct CancelBy<'a> {
    pub(crate) by: &'a Identity,
    pub(crate) name: &'a str,
    pub(crate) force: bool,
    pub(crate) code: ReasonCode,
    pub(crate) reason: Option<&'a str>,
}

fn error(code: ProtocolErrorCode, text: impl Into<String>) -> CliReply {
    CliReply::Error(refusal(code, text))
}

impl Engine {
    pub(crate) async fn on_cli(&mut self, e: CliEvent) {
        let reply = e.reply.clone();
        let answer = match e.req {
            CliRequest::Submit(req) => {
                self.submit(e.conn, &e.os_user, req, &e.reply, &e.hangup)
                    .await
            }
            CliRequest::Done { job } => self.done(e.conn, &job),
            CliRequest::Wait { job } => self.wait(e.conn, &job, &e.reply, &e.hangup),
            CliRequest::Cancel(req) => Some(self.cancel(&e.os_user, req).await),
            CliRequest::Read(r) => self.read(r, &e.reply),
            CliRequest::Brokers => Some(self.brokers().await),
            CliRequest::Jobs { label, active } => Some(self.jobs(&e.os_user, &label, active)),
            CliRequest::Orders => Some(self.orders(&e.os_user)),
            CliRequest::Shutdown => None,
            CliRequest::LoggingReload => Some(self.reload_logging()),
            CliRequest::Events => {
                crate::bus::watch(&self.s, e.reply.clone());
                None
            }
        };
        if let Some(a) = answer {
            if reply.try_send(a).is_err() {
                self.s.overflow(
                    Overflow::CliReply,
                    Level::Warn,
                    "CLI reply dropped: client not reading",
                    &[],
                );
            }
        }
    }

    async fn submit(
        &mut self,
        conn: ConnId,
        os_user: &str,
        mut req: SubmitRequest,
        reply: &mpsc::Sender<CliReply>,
        hangup: &Arc<Notify>,
    ) -> Option<CliReply> {
        req.identity.os_user = os_user.to_string();
        let now = self.now();
        let built = match build(&req, self.s.files.as_ref(), self.s.repository.as_ref(), now).await
        {
            Ok(b) => b,
            Err(Refusal::Invalid(m)) => return Some(error(ProtocolErrorCode::Malformed, m)),
            Err(Refusal::Rejected(code, detail)) => {
                return Some(CliReply::Rejected { code, detail })
            }
        };
        let (spec, warnings) = (built.spec, built.warnings);
        if let Some(e) = &built.dirty_check_failed {
            self.s.diag(
                Level::Warn,
                "uncommitted changes not checked; the order goes without the warning",
                &[("error", e)],
            );
        }
        if spec.requirements.same_machine.is_some() && !self.links.contains_key(LOCAL_BROKER) {
            return Some(CliReply::Rejected {
                code: ReasonCode::Rejected(RejectReason::NoLocalBroker),
                detail:
                    "заказ на эту машину, а локальный брокер не настроен: запустите localrunner up"
                        .into(),
            });
        }
        let job = self.ids.next(now);
        let id = &req.identity;
        let (handle, log) = match (handle_path(&id.cwd, &job), log_path(&id.cwd, &job)) {
            (Ok(h), Ok(l)) => (h, l),
            (Err(m), _) | (_, Err(m)) => return Some(error(ProtocolErrorCode::Malformed, m)),
        };
        if let Err(e) = self.s.store.raise_job_id_floor(self.ids.last_ms()).await {
            self.store_failed("storage failed: job-id floor not raised", &job, &e);
        }
        let mut order = ActiveOrder::new(
            job.clone(),
            &id.label,
            os_user,
            id.sender_pid,
            id.sender_start,
        );
        order.handle_path = Some(handle.display().to_string());
        order.detached = req.detach;
        let (payload, spec_json) = match (
            serde_json::to_string(&order_payload(&job, &spec)),
            serde_json::to_string(&spec),
        ) {
            (Ok(p), Ok(s)) => (p, s),
            (Err(e), _) | (_, Err(e)) => {
                return Some(error(ProtocolErrorCode::Malformed, e.to_string()))
            }
        };
        order.spec_json = Some(spec_json);
        let stored = self
            .s
            .store
            .enqueue(order.clone(), OutboxKind::Order, payload, now)
            .await;
        if let Err(e) = stored {
            self.store_failed("storage failed: order not queued", &job, &e);
            return Some(error(ProtocolErrorCode::Unavailable, "hub storage failed"));
        }
        let files = self.s.files.as_ref();
        let written = write_handle(
            files,
            &handle,
            &job,
            &id.label,
            &spec.profile,
            now,
            req.detach,
        )
        .await;
        if let Err(m) = written {
            if let Err(e) = self.s.store.finish(&job).await {
                self.store_failed(
                    "storage failed: order without handle stays in hub.db",
                    &job,
                    &e,
                );
            }
            return Some(error(
                ProtocolErrorCode::Unavailable,
                format!("handle {} not written: {m}", handle.display()),
            ));
        }
        let mut header = crate::logs::header(&job, &spec);
        for w in &warnings {
            header.push_str(&format!("=== {w} ===\n"));
        }
        let mut track = Track::new(order, Some(spec), Stage::Selecting);
        track.log = Some(log);
        if req.wait {
            track.watchers.push((conn, reply.clone(), hangup.clone()));
        }
        self.tracks.insert(job.clone(), track);
        self.log_copy(&job, &header).await;
        self.submitted.entry(conn).or_default().insert(job.clone());
        if reply
            .try_send(CliReply::Submitted {
                job: job.clone(),
                warnings,
            })
            .is_err()
        {
            self.s
                .diag(Level::Warn, "CLI reply dropped: client not reading", &[]);
        }
        self.start_selection(&job);
        None
    }

    fn reload_logging(&self) -> CliReply {
        let Some(reload) = &self.s.reload_logging else {
            return error(
                ProtocolErrorCode::Unavailable,
                "this hub cannot reload its logging configuration",
            );
        };
        match reload() {
            Ok(()) => {
                self.s
                    .diag(Level::Info, "logging configuration reloaded", &[]);
                CliReply::Reloaded
            }
            Err(e) => error(ProtocolErrorCode::Malformed, e),
        }
    }

    fn done(&mut self, conn: ConnId, job: &JobId) -> Option<CliReply> {
        let ours = self.submitted.get(&conn).is_some_and(|s| s.contains(job));
        if ours {
            return None;
        }
        Some(error(
            ProtocolErrorCode::Malformed,
            format!("job {job} was not submitted on this connection"),
        ))
    }

    fn wait(
        &mut self,
        conn: ConnId,
        job: &JobId,
        reply: &mpsc::Sender<CliReply>,
        hangup: &Arc<Notify>,
    ) -> Option<CliReply> {
        if let Some(t) = self.tracks.get_mut(job) {
            t.watchers.push((conn, reply.clone(), hangup.clone()));
            return None;
        }
        if let Some((_, f)) = self.recent.iter().find(|(j, _)| j == job) {
            return Some(f.reply.clone());
        }
        Some(error(
            ProtocolErrorCode::Malformed,
            format!("job {job} is not known to this hub"),
        ))
    }

    async fn cancel(&mut self, os_user: &str, req: CancelRequest) -> CliReply {
        let mut actor = req.actor;
        actor.os_user = os_user.to_string();
        let jobs: Vec<JobId> = match &req.target {
            CancelTarget::Job(job) => {
                let Some(t) = self.tracks.get(job) else {
                    return error(
                        ProtocolErrorCode::Malformed,
                        format!("job {job} is not active on this hub"),
                    );
                };
                if !t.owned_by(os_user, &actor.label) && !req.force {
                    return error(
                        ProtocolErrorCode::Unauthorized,
                        format!(
                            "job {job} belongs to label {}; cancel it with --force",
                            t.order.label
                        ),
                    );
                }
                vec![job.clone()]
            }
            CancelTarget::Mine => self
                .tracks
                .values()
                .filter(|t| t.owned_by(os_user, &actor.label) && !t.cancelled)
                .map(|t| t.order.job.clone())
                .collect(),
        };
        for job in &jobs {
            let by = CancelBy {
                by: &actor,
                name: &actor.label,
                force: req.force,
                code: ReasonCode::CancelledByUser,
                reason: req.reason.as_deref(),
            };
            self.cancel_job(job, by).await;
        }
        CliReply::Cancelled { jobs }
    }

    pub(crate) async fn cancel_job(&mut self, job: &JobId, c: CancelBy<'_>) {
        if self.tracks.get(job).is_none_or(|t| t.cancelled) {
            return;
        }
        let now = self.now();
        let line = crate::logs::cancel_line(c.name, now, c.force, &c.code, c.reason);
        self.log_copy(job, &line).await;
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        track.cancelled = true;
        match track.stage {
            Stage::Selecting | Stage::Waiting => {
                track.gen = track.gen.wrapping_add(1);
                self.final_result(job, JobState::Cancelled, None, Some(c.code))
                    .await;
            }
            Stage::Sent => {
                if let Err(e) = self.s.store.discard_outbox(job).await {
                    self.store_failed("storage failed: order entry not discarded", job, &e);
                }
                self.push_cancel(job, c.by, c.force, c.code, c.reason).await;
            }
            Stage::Accepted | Stage::Started => {
                self.push_cancel(job, c.by, c.force, c.code, c.reason).await;
            }
        }
    }

    fn read(&mut self, r: CliRead, reply: &mpsc::Sender<CliReply>) -> Option<CliReply> {
        if matches!(
            r.request,
            ReadRequest::Subscribe { .. }
                | ReadRequest::Unsubscribe { .. }
                | ReadRequest::Unfollow { .. }
        ) {
            return Some(error(
                ProtocolErrorCode::Malformed,
                "subscriptions on the hub's broker connections are managed by the hub",
            ));
        }
        let targets: Vec<(String, mpsc::Sender<lr_abstractions::Frame>)> = self
            .links
            .iter()
            .filter(|(n, _)| r.broker.as_deref().is_none_or(|b| b == n.as_str()))
            .filter_map(|(n, l)| l.out.clone().map(|o| (n.clone(), o)))
            .collect();
        if targets.is_empty() {
            return Some(error(
                ProtocolErrorCode::Unavailable,
                "no such broker is connected",
            ));
        }
        let wait = Duration::from_secs(self.policy.brokers.query_timeout_s);
        if let ReadRequest::Log { follow, .. } = &r.request {
            for (broker, out) in targets {
                let read = LogRead {
                    broker,
                    out,
                    request: r.request.clone(),
                    follow: *follow,
                    wait,
                };
                relay_log(self.s.clone(), read, reply.clone());
            }
            return None;
        }
        for (name, out) in targets {
            let s = self.s.clone();
            let (reply, request) = (reply.clone(), r.request.clone());
            tokio::spawn(async move {
                let req = s.next_req();
                let rx = s.pending.register(req);
                let sent =
                    lr_abstractions::Frame::json(&HubToBroker::Read(ReadCall { req, request }))
                        .is_ok_and(|f| out.try_send(f).is_ok());
                let answer = if sent {
                    tokio::select! {
                        biased;
                        a = rx => a.ok(),
                        _ = s.clock.sleep(wait) => None,
                    }
                } else {
                    None
                };
                s.pending.forget(req);
                let msg = match answer {
                    Some(lr_proto::BrokerToHub::ReadReply(a)) => CliReply::Read {
                        broker: name,
                        reply: a.reply,
                    },
                    _ => error(
                        ProtocolErrorCode::Unavailable,
                        format!("broker {name} did not answer"),
                    ),
                };
                let _ = reply.try_send(msg);
            });
        }
        None
    }

    async fn brokers(&mut self) -> CliReply {
        let pending = match self.s.store.pending_outbox().await {
            Ok(p) => p,
            Err(e) => {
                return error(
                    ProtocolErrorCode::Unavailable,
                    format!("outbox unreadable: {e}"),
                )
            }
        };
        let brokers = self
            .links
            .iter()
            .map(|(name, l)| {
                let outbox = pending
                    .iter()
                    .filter(|e| {
                        self.tracks
                            .get(&e.job)
                            .is_some_and(|t| t.order.broker.as_deref() == Some(name.as_str()))
                    })
                    .count();
                let zone = match name.as_str() {
                    LOCAL_BROKER => Some(lr_core::Zone::Local),
                    _ => l.zone,
                };
                BrokerStatus {
                    name: name.clone(),
                    zone,
                    available: l.out.is_some(),
                    outbox: u64::try_from(outbox).unwrap_or(u64::MAX),
                }
            })
            .collect();
        CliReply::Brokers { brokers }
    }

    fn jobs(&self, os_user: &str, label: &str, active: bool) -> CliReply {
        let mut jobs: Vec<lr_proto::JobSummary> = self
            .tracks
            .values()
            .filter(|t| t.owned_by(os_user, label))
            .map(|t| t.summary(t.state()))
            .collect();
        if !active {
            jobs.extend(
                self.recent
                    .iter()
                    .filter(|(_, f)| f.os_user == os_user && f.label == label)
                    .map(|(_, f)| f.summary.clone()),
            );
        }
        jobs.sort_by(|a, b| (a.queued_at, &a.job).cmp(&(b.queued_at, &b.job)));
        CliReply::Jobs { jobs }
    }

    fn orders(&self, os_user: &str) -> CliReply {
        let mut orders: Vec<HubOrder> = self
            .tracks
            .values()
            .filter(|t| t.order.os_user == os_user)
            .map(|t| HubOrder {
                broker: t.order.broker.clone(),
                outbox: !t.stage.accepted(),
                summary: t.summary(t.state()),
                log: t.log.as_ref().map(|p| p.display().to_string()),
            })
            .collect();
        orders.sort_by(|a, b| {
            (a.summary.queued_at, &a.summary.job).cmp(&(b.summary.queued_at, &b.summary.job))
        });
        CliReply::orders_within_frame(orders)
    }
}
