use crate::engine::{Engine, Event, LOCAL_BROKER, MOVE_CHECK_S, MOVE_PAUSE_S};
use crate::limits::{Overflow, RECENT_MAX};
use crate::orders::order_payload;
use crate::select::{allowed_zones, broker_zone, Candidate, Choice, Decision};
use crate::track::{Finished, Stage};
use lr_abstractions::{Frame, Level, OutboxKind};
use lr_core::{JobId, JobState, Timestamp};
use lr_proto::{CliReply, HubToBroker, ProtocolErrorCode, ReasonCode, RejectReason, Withdraw};

pub(crate) const UP_HINT: &str = "no runner on your installation, start it: localrunner up";

impl Engine {
    pub(crate) fn start_selection(&mut self, job: &JobId) {
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        let Some(spec) = track.spec.clone() else {
            return;
        };
        track.gen = track.gen.wrapping_add(1);
        track.stage = Stage::Selecting;
        track.next_select = None;
        let gen = track.gen;
        let candidates: Vec<Candidate> = self
            .links
            .iter()
            .filter(|(n, _)| !track.excluded.contains(*n))
            .map(|(n, l)| Candidate {
                name: n.clone(),
                zone: broker_zone(n, l.zone),
                out: l.out.clone(),
            })
            .collect();
        let allowed = allowed_zones(&spec.requirements);
        let any_allowed = candidates.iter().any(|c| allowed.contains(&c.zone));
        if !any_allowed && !track.excluded.is_empty() {
            let (code, detail) = track.last_rejection.clone().unwrap_or((
                ReasonCode::Rejected(RejectReason::NoMatchingRunner),
                "all brokers refused".into(),
            ));
            let (tx, job) = (self.tx.clone(), job.clone());
            let decision = Decision::Reject(code, detail);
            tokio::spawn(async move {
                let _ = tx.send(Event::Selected { job, gen, decision }).await;
            });
            return;
        }
        let choice = Choice {
            policy: self.policy.brokers.clone(),
            req: spec.requirements.clone(),
            probe: Some(Box::new(spec)),
            candidates,
            shared: self.s.clone(),
        };
        let (tx, job) = (self.tx.clone(), job.clone());
        tokio::spawn(async move {
            let decision = choice.run().await;
            let _ = tx.send(Event::Selected { job, gen, decision }).await;
        });
    }

    /// Looks for waiting orders that another broker would start now: asks the allowed brokers
    /// about each and gives the order back from the one that holds it (spec 3.1).
    pub(crate) fn check_moves(&mut self, now: Timestamp) {
        let jobs: Vec<JobId> = self
            .tracks
            .iter()
            .filter(|(_, t)| {
                t.stage == Stage::Accepted
                    && !t.cancelled
                    && t.spec.is_some()
                    && !t.local_only()
                    && t.moved_until.is_none_or(|at| at <= now)
                    && t.order
                        .broker
                        .as_ref()
                        .and_then(|b| self.links.get(b))
                        .is_some_and(|l| l.out.is_some())
            })
            .map(|(j, _)| j.clone())
            .collect();
        for job in jobs {
            let Some(track) = self.tracks.get_mut(&job) else {
                continue;
            };
            let (Some(spec), Some(from)) = (track.spec.clone(), track.order.broker.clone()) else {
                continue;
            };
            track.moved_until = Some(now.plus_secs(MOVE_CHECK_S));
            let gen = track.gen;
            let candidates: Vec<Candidate> = self
                .links
                .iter()
                .filter(|(n, _)| !track.excluded.contains(*n))
                .map(|(n, l)| Candidate {
                    name: n.clone(),
                    zone: broker_zone(n, l.zone),
                    out: l.out.clone(),
                })
                .collect();
            let choice = Choice {
                policy: self.policy.brokers.clone(),
                req: spec.requirements.clone(),
                probe: Some(Box::new(spec)),
                candidates,
                shared: self.s.clone(),
            };
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let target = choice.move_target(&from).await;
                let _ = tx
                    .send(Event::MoveTarget {
                        job,
                        gen,
                        from,
                        target,
                    })
                    .await;
            });
        }
    }

    pub(crate) async fn on_move_target(
        &mut self,
        job: &JobId,
        gen: u64,
        from: &str,
        target: Option<String>,
    ) {
        let now = self.now();
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        let Some(target) = target else {
            return;
        };
        if track.gen != gen
            || track.stage != Stage::Accepted
            || track.cancelled
            || track.order.broker.as_deref() != Some(from)
        {
            return;
        }
        let Some(out) = self.links.get(from).and_then(|l| l.out.clone()) else {
            return;
        };
        let msg = HubToBroker::Withdraw(Withdraw { job: job.clone() });
        let Ok(frame) = Frame::json(&msg) else {
            return;
        };
        if out.try_send(frame).is_err() {
            return;
        }
        track.moved_until = Some(now.plus_secs(MOVE_PAUSE_S));
        self.s.diag(
            Level::Info,
            "a waiting order is asked back to start at another broker",
            &[("job", &job.0), ("from", from), ("target", &target)],
        );
    }

    fn locally_refused(&self) -> Option<String> {
        self.links
            .get(LOCAL_BROKER)
            .and_then(|l| l.refused.as_ref())
            .filter(|e| e.code == ProtocolErrorCode::Unauthorized)
            .map(|e| e.message.clone())
    }

    pub(crate) async fn on_decision(&mut self, job: &JobId, gen: u64, decision: Decision) {
        let refused = self.locally_refused();
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        if track.gen != gen || track.stage != Stage::Selecting {
            return;
        }
        match decision {
            Decision::Chosen(name) => {
                if let Err(e) = self.s.store.set_broker(job, Some(&name)).await {
                    let pause = self.policy.brokers.query_retry_s.max(1);
                    track.stage = Stage::Waiting;
                    track.wait_reason = Some("the broker choice was not saved, retrying".into());
                    track.next_select = Some(self.s.clock.now().plus_secs(pause));
                    self.store_failed("storage failed: broker choice not recorded", job, &e);
                    return;
                }
                track.order.broker = Some(name.clone());
                track.stage = Stage::Sent;
                track.wait_reason = None;
                self.flush(&name).await;
            }
            Decision::Wait(reason) => {
                if let (true, Some(why)) = (track.local_only(), refused) {
                    let code = ReasonCode::Rejected(RejectReason::LocalUserUnknown);
                    return self.final_reject(job, code, why).await;
                }
                track.stage = Stage::Waiting;
                track.wait_reason = Some(reason);
                let pause = self.policy.brokers.query_retry_s.max(1);
                track.next_select = Some(self.s.clock.now().plus_secs(pause));
            }
            Decision::Reject(code, detail) => self.final_reject(job, code, detail).await,
        }
    }

    pub(crate) async fn local_user_refused(&mut self) {
        let Some(why) = self.locally_refused() else {
            return;
        };
        let jobs: Vec<JobId> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.local_only() && matches!(t.stage, Stage::Selecting | Stage::Waiting))
            .map(|(j, _)| j.clone())
            .collect();
        for job in jobs {
            let code = ReasonCode::Rejected(RejectReason::LocalUserUnknown);
            self.final_reject(&job, code, why.clone()).await;
        }
    }

    pub(crate) async fn rebroker(
        &mut self,
        job: &JobId,
        from: &str,
        code: ReasonCode,
        detail: String,
    ) {
        if code == ReasonCode::Moved {
            let line =
                format!("=== moved: {from} gave the order back, it starts elsewhere now ===\n");
            self.log_copy(job, &line).await;
        }
        let Some(track) = self.tracks.get_mut(job) else {
            return;
        };
        if track.cancelled {
            let reason = Some(ReasonCode::CancelledByUser);
            return self
                .final_result(job, JobState::Cancelled, None, reason)
                .await;
        }
        let Some(spec) = track.spec.clone().filter(|_| !track.local_only()) else {
            let detail = if track.local_only() {
                format!("{detail}; {UP_HINT}")
            } else {
                detail
            };
            return self.final_reject(job, code, detail).await;
        };
        let payload = match serde_json::to_string(&order_payload(job, &spec)) {
            Ok(p) => p,
            Err(e) => {
                self.s.diag(
                    Level::Error,
                    "order not serialisable",
                    &[("job", &job.0), ("error", &e.to_string())],
                );
                return;
            }
        };
        let now = self.s.clock.now();
        if let Err(e) = self
            .s
            .store
            .rebroker(job, OutboxKind::Order, payload, now)
            .await
        {
            self.store_failed("storage failed: order not re-brokered", job, &e);
            return;
        }
        if let Some(l) = self.links.get_mut(from) {
            l.forget(job);
        }
        if code != ReasonCode::Moved {
            track.excluded.insert(from.to_string());
            track.last_rejection = Some((code, detail));
        }
        track.order.broker = None;
        track.order.last_seq = 0;
        track.runner = None;
        self.start_selection(job);
    }

    async fn finish(&mut self, job: &JobId, line: String, reply: CliReply, state: JobState) {
        self.log_copy(job, &line).await;
        self.notify(job, &reply);
        if let Err(e) = self.s.store.finish(job).await {
            self.store_failed("storage failed: finished job stays in hub.db", job, &e);
        }
        for l in self.links.values_mut() {
            l.forget(job);
        }
        let Some(track) = self.tracks.remove(job) else {
            return;
        };
        let mut summary = track.summary(state);
        match &reply {
            CliReply::Exit {
                exit_code, reason, ..
            } => {
                summary.exit_code = *exit_code;
                summary.reason = reason.clone();
            }
            CliReply::Rejected { code, .. } => summary.reason = Some(code.clone()),
            _ => {}
        }
        if self.recent.len() >= RECENT_MAX {
            if let Some((old, _)) = self.recent.pop_front() {
                self.s.overflow(
                    Overflow::Result,
                    Level::Debug,
                    "finished job forgotten: more than 1024 results in memory",
                    &[("job", &old.0)],
                );
            }
        }
        self.recent.push_back((
            job.clone(),
            Finished {
                label: track.order.label.clone(),
                os_user: track.order.os_user.clone(),
                summary,
                reply,
            },
        ));
    }

    pub(crate) async fn final_reject(&mut self, job: &JobId, code: ReasonCode, detail: String) {
        let line = crate::logs::rejected_line(&code, &detail);
        let reply = CliReply::Rejected { code, detail };
        self.finish(job, line, reply, JobState::Cancelled).await;
    }

    pub(crate) async fn final_result(
        &mut self,
        job: &JobId,
        state: JobState,
        exit_code: Option<i32>,
        reason: Option<ReasonCode>,
    ) {
        let line = crate::logs::result_line(state, exit_code, reason.as_ref());
        let reply = CliReply::Exit {
            job: job.clone(),
            state,
            exit_code,
            reason,
        };
        self.finish(job, line, reply, state).await;
    }
}
