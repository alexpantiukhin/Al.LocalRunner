use crate::agent::{after, apply_control, Control, JobSlot, Shared};
use crate::job::{on_assign, on_cancel};
use crate::outbox::Replay;
use crate::settings::MAX_HEARTBEAT_S;
use lr_abstractions::{Frame, Level, Stop, Transport};
use lr_core::{Endpoint, JobId};
use lr_proto::{
    receive, Ack, BrokerToRunner, Bye, CapabilitiesUpdate, Hello, PeerKind, ProtocolError,
    ProtocolErrorCode, ReasonCode, Received, Resume, ResumePoint, RunnerToBroker,
    HEARTBEAT_INTERVAL_S, MISSED_HEARTBEATS_OFFLINE, PROTOCOL_VERSION,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub(crate) enum End {
    Lost { welcomed: bool },
    Refused(ProtocolError),
    Stopped,
}

enum Step {
    Go,
    End(End),
}

pub(crate) struct Session<'a> {
    pub(crate) shared: &'a Arc<Shared>,
    pub(crate) t: &'a dyn Transport,
    pub(crate) slot: &'a mut JobSlot,
    replay: Replay,
    kind: PeerKind,
    limit: Duration,
}

async fn send_within(s: &Shared, t: &dyn Transport, frame: Frame, limit: Duration) -> bool {
    tokio::select! {
        r = t.send(frame) => r.is_ok(),
        _ = s.deps.clock.sleep(limit) => {
            s.diag.event(Level::Warn, "broker stopped reading, reconnecting", &[]);
            t.close().await;
            false
        }
    }
}

impl Session<'_> {
    pub(crate) async fn send(&self, msg: &RunnerToBroker) -> bool {
        match Frame::json(msg) {
            Ok(f) => send_within(self.shared, self.t, f, self.limit).await,
            Err(_) => false,
        }
    }

    pub(crate) async fn ack(&self, job: &JobId, seq: u64) -> bool {
        self.send(&RunnerToBroker::Ack(Ack {
            job: job.clone(),
            seq,
        }))
        .await
    }

    pub(crate) async fn refuse_gap(&self, job: &JobId, expected: u64, got: u64) -> bool {
        self.send(&RunnerToBroker::Error(ProtocolError {
            code: ProtocolErrorCode::SeqGap,
            message: format!("job {job}: expected seq {expected}, got {got}"),
            supported: None,
        }))
        .await
    }

    async fn flush(&mut self) -> bool {
        let services = std::mem::take(&mut self.shared.state().services.dirty);
        if services {
            let report = self.shared.services().report(self.shared.now()).await;
            if !self.send(&RunnerToBroker::ServicesReport(report)).await {
                return false;
            }
        }
        for frame in self.replay.frames(&self.shared.outbox).await {
            if !send_within(self.shared, self.t, frame, self.limit).await {
                return false;
            }
        }
        let dirty = std::mem::take(&mut self.shared.state().caps_dirty);
        if dirty {
            let capabilities = self.shared.capabilities().await;
            return self
                .send(&RunnerToBroker::CapabilitiesUpdate(CapabilitiesUpdate {
                    capabilities,
                }))
                .await;
        }
        true
    }

    async fn resume_points(&self) -> Vec<ResumePoint> {
        let d = &self.shared.deps;
        let mut jobs: BTreeSet<JobId> = match d.store.pending_outbox().await {
            Ok(p) => p.into_iter().map(|e| e.job).collect(),
            Err(e) => {
                self.shared.diag.event(
                    Level::Error,
                    "outbox unreadable for resume",
                    &[("error", &e.to_string())],
                );
                BTreeSet::new()
            }
        };
        if let Some(cur) = self.shared.state().job.as_ref() {
            jobs.insert(cur.job.clone());
        }
        let mut points = Vec::new();
        for job in jobs {
            match d.store.last_seq(&job).await {
                Ok(last_seq) => points.push(ResumePoint { job, last_seq }),
                Err(e) => self.shared.diag.event(
                    Level::Error,
                    "received seq unreadable",
                    &[("job", &job.0), ("error", &e.to_string())],
                ),
            }
        }
        points
    }

    async fn idle_and_stopping(&self) -> bool {
        let s = self.shared;
        if !s.state().draining || s.state().job.is_some() {
            return false;
        }
        matches!(s.deps.store.pending_outbox().await, Ok(p) if p.is_empty())
    }

    async fn refused(&mut self, e: ProtocolError) -> End {
        give_up(self.shared, self.slot, &e).await;
        self.t.close().await;
        End::Refused(e)
    }

    async fn handle(&mut self, frame: Frame) -> Step {
        let msg = match receive::<BrokerToRunner>(frame.as_bytes(), self.kind) {
            Ok(Received::Message(m)) => m,
            Ok(Received::Ignored(t)) => {
                self.shared.diag.event(
                    Level::Debug,
                    "unknown message type skipped",
                    &[("type", &t)],
                );
                return Step::Go;
            }
            Err(e) => {
                self.shared.diag.event(
                    Level::Warn,
                    "malformed message from the broker",
                    &[("error", &e.to_string())],
                );
                if let Some(r) = e.refusal() {
                    self.send(&RunnerToBroker::Error(r)).await;
                }
                return Step::Go;
            }
        };
        let d = &self.shared.deps;
        match msg {
            BrokerToRunner::Ack(a) => {
                if let Err(e) = d.store.ack_outbox(&a.job, a.seq).await {
                    self.shared.diag.event(
                        Level::Error,
                        "ack not stored",
                        &[("job", &a.job.0), ("error", &e.to_string())],
                    );
                }
                self.shared.wake.notify_one();
            }
            BrokerToRunner::Resume(r) => {
                for p in r.points.iter().filter(|p| p.last_seq > 0) {
                    if let Err(e) = d.store.ack_outbox(&p.job, p.last_seq).await {
                        self.shared.diag.event(
                            Level::Error,
                            "resume not applied to the outbox",
                            &[("job", &p.job.0), ("error", &e.to_string())],
                        );
                    }
                }
            }
            BrokerToRunner::Assign(a) => on_assign(self, a).await,
            BrokerToRunner::Cancel(c) => on_cancel(self, c).await,
            BrokerToRunner::StopService(s) => self.shared.stop_service_detached(s.name),
            BrokerToRunner::SettingsUpdate(u) => {
                self.shared.set_broker_lost_kill(u.broker_lost_kill_s);
            }
            BrokerToRunner::Error(e) if e.code.kills_running_jobs() => {
                return Step::End(self.refused(e).await)
            }
            BrokerToRunner::Error(e) if e.code == ProtocolErrorCode::SeqGap => {
                self.shared
                    .diag
                    .event(Level::Warn, "broker saw a seq gap, reconnecting", &[]);
                self.t.close().await;
                return Step::End(End::Lost { welcomed: true });
            }
            BrokerToRunner::Error(e) => self.shared.diag.event(
                Level::Warn,
                "broker reported an error",
                &[("code", &format!("{:?}", e.code)), ("message", &e.message)],
            ),
            BrokerToRunner::Bye(_) => return Step::End(End::Lost { welcomed: true }),
            BrokerToRunner::Heartbeat | BrokerToRunner::Welcome(_) | BrokerToRunner::Joined(_) => {}
        }
        Step::Go
    }
}

fn peer_kind(t: &dyn Transport) -> PeerKind {
    match t.peer().endpoint {
        Endpoint::Ipc { .. } => PeerKind::Ipc,
        Endpoint::Addr(_) => PeerKind::Network,
    }
}

async fn hello(s: &Shared, t: &dyn Transport) -> Result<Duration, End> {
    let lost = End::Lost { welcomed: false };
    let capabilities = s.capabilities().await;
    let msg = RunnerToBroker::Hello(Hello {
        version: PROTOCOL_VERSION,
        name: s.deps.settings.name.clone(),
        capabilities: Some(capabilities),
    });
    let frame = Frame::json(&msg).map_err(|_| End::Lost { welcomed: false })?;
    let wait = Duration::from_secs(HEARTBEAT_INTERVAL_S * u64::from(MISSED_HEARTBEATS_OFFLINE));
    if !send_within(s, t, frame, wait).await {
        return Err(lost);
    }
    let frame = tokio::select! {
        f = t.recv() => f.map_err(|_| End::Lost { welcomed: false })?,
        _ = s.deps.clock.sleep(wait) => return Err(lost),
    };
    match receive::<BrokerToRunner>(frame.as_bytes(), peer_kind(t)) {
        Ok(Received::Message(BrokerToRunner::Welcome(w))) => {
            s.set_broker_lost_kill(w.broker_lost_kill_s);
            s.set_lost(None);
            s.diag.event(
                Level::Info,
                "broker connected",
                &[("broker", &s.deps.broker.address)],
            );
            Ok(Duration::from_secs(w.heartbeat_s.clamp(1, MAX_HEARTBEAT_S)))
        }
        Ok(Received::Message(BrokerToRunner::Error(e))) if e.code.kills_running_jobs() => {
            Err(End::Refused(e))
        }
        Ok(Received::Message(BrokerToRunner::Error(e))) => {
            s.diag.event(
                Level::Warn,
                "broker refused the hello",
                &[("code", &format!("{:?}", e.code)), ("message", &e.message)],
            );
            Err(lost)
        }
        _ => Err(lost),
    }
}

pub(crate) async fn give_up(shared: &Arc<Shared>, slot: &mut JobSlot, e: &ProtocolError) {
    shared.diag.event(
        Level::Error,
        "broker refused authorization, job killed and outbox dropped",
        &[("code", &format!("{:?}", e.code))],
    );
    shared.stop_job(
        Stop::Kill,
        ReasonCode::RunnerRemoved,
        "authorization refused",
    );
    slot.wait().await;
    shared.stop_all_services().await;
    if let Err(err) = shared.deps.store.clear_outbox().await {
        shared.diag.event(
            Level::Error,
            "outbox not cleared",
            &[("error", &err.to_string())],
        );
    }
}

pub(crate) async fn session(
    shared: &Arc<Shared>,
    t: &dyn Transport,
    slot: &mut JobSlot,
    control: &mut mpsc::Receiver<Control>,
) -> End {
    let heartbeat = match hello(shared, t).await {
        Ok(h) => h,
        Err(End::Refused(e)) => {
            let mut s = Session {
                shared,
                t,
                slot,
                replay: Replay::default(),
                kind: peer_kind(t),
                limit: Duration::from_secs(HEARTBEAT_INTERVAL_S),
            };
            return s.refused(e).await;
        }
        Err(end) => return end,
    };
    let silence = heartbeat.saturating_mul(MISSED_HEARTBEATS_OFFLINE);
    let mut s = Session {
        shared,
        t,
        slot,
        replay: Replay::default(),
        kind: peer_kind(t),
        limit: silence,
    };
    let lost = || End::Lost { welcomed: true };
    let points = s.resume_points().await;
    let report = shared.services().report(shared.now()).await;
    if !s.send(&RunnerToBroker::Resume(Resume { points })).await
        || !s.send(&RunnerToBroker::ServicesReport(report)).await
        || !s.flush().await
    {
        return lost();
    }
    if shared.state().draining && !s.send(&RunnerToBroker::Drain).await {
        return lost();
    }
    let clock = shared.deps.clock.clone();
    let mut next_beat = after(shared.now(), heartbeat);
    let mut last_rx = shared.now();
    loop {
        if s.idle_and_stopping().await && shared.services_down() {
            s.flush().await;
            s.send(&RunnerToBroker::Bye(Bye {
                reason: Some("runner stopped".into()),
            }))
            .await;
            t.close().await;
            return End::Stopped;
        }
        let deadline = after(last_rx, silence);
        tokio::select! {
            f = t.recv() => match f {
                Ok(frame) => {
                    last_rx = shared.now();
                    if let Step::End(end) = s.handle(frame).await {
                        return end;
                    }
                }
                Err(_) => return lost(),
            },
            _ = shared.wake.notified() => {
                if !s.flush().await {
                    return lost();
                }
            }
            _ = clock.sleep_until(next_beat) => {
                next_beat = after(next_beat, heartbeat);
                if !s.send(&RunnerToBroker::Heartbeat).await {
                    return lost();
                }
            }
            _ = clock.sleep_until(deadline) => {
                shared.diag.event(Level::Warn, "broker silent for three heartbeats, reconnecting", &[]);
                t.close().await;
                return lost();
            }
            cmd = control.recv() => {
                if let Some(cmd) = cmd {
                    apply_control(shared, cmd);
                    if !s.send(&RunnerToBroker::Drain).await {
                        return lost();
                    }
                }
            }
        }
    }
}
