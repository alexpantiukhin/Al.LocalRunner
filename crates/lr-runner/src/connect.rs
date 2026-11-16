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
use std::collections::{BTreeSet, VecDeque};
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

struct Inbox {
    frames: mpsc::Receiver<Frame>,
    held: VecDeque<Frame>,
    heard: bool,
}

impl Inbox {
    async fn next(&mut self) -> Option<Frame> {
        if let Some(frame) = self.held.pop_front() {
            return Some(frame);
        }
        self.frames.recv().await
    }

    fn take_heard(&mut self) -> bool {
        std::mem::take(&mut self.heard)
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

    /// Sends one frame while reading what the broker says. The broker acknowledges every job
    /// frame and stops reading the runner once its queue of acknowledgements for this peer is
    /// full, so a sender that does not read them deadlocks against it. Acknowledgements are
    /// applied at once; other frames wait in `inbox` until the send is over, because handling
    /// them may need to send, and the write in flight must neither be cut nor overtaken.
    async fn send_reading(&self, frame: Frame, inbox: &mut Inbox) -> bool {
        let send = send_within(self.shared, self.t, frame, self.limit);
        tokio::pin!(send);
        loop {
            tokio::select! {
                sent = &mut send => return sent,
                got = inbox.frames.recv() => match got {
                    Some(frame) => {
                        inbox.heard = true;
                        self.absorb(frame, inbox).await;
                    }
                    None => return false,
                },
            }
        }
    }

    async fn send_msg_reading(&self, msg: &RunnerToBroker, inbox: &mut Inbox) -> bool {
        match Frame::json(msg) {
            Ok(f) => self.send_reading(f, inbox).await,
            Err(_) => false,
        }
    }

    async fn absorb(&self, frame: Frame, inbox: &mut Inbox) {
        match receive::<BrokerToRunner>(frame.as_bytes(), self.kind) {
            Ok(Received::Message(BrokerToRunner::Ack(a))) => self.on_ack(&a).await,
            Ok(Received::Message(BrokerToRunner::Resume(r))) => self.on_resume(&r).await,
            _ => inbox.held.push_back(frame),
        }
    }

    async fn on_ack(&self, a: &Ack) {
        if let Err(e) = self.shared.deps.store.ack_outbox(&a.job, a.seq).await {
            self.shared.diag.event(
                Level::Error,
                "ack not stored",
                &[("job", &a.job.0), ("error", &e.to_string())],
            );
        }
        self.shared.wake.notify_one();
    }

    async fn on_resume(&self, r: &Resume) {
        for p in r.points.iter().filter(|p| p.last_seq > 0) {
            if let Err(e) = self.shared.deps.store.ack_outbox(&p.job, p.last_seq).await {
                self.shared.diag.event(
                    Level::Error,
                    "resume not applied to the outbox",
                    &[("job", &p.job.0), ("error", &e.to_string())],
                );
            }
        }
    }

    async fn flush(&mut self, inbox: &mut Inbox) -> bool {
        let services = std::mem::take(&mut self.shared.state().services.dirty);
        if services {
            let report = self.shared.services().report(self.shared.now()).await;
            if !self
                .send_msg_reading(&RunnerToBroker::ServicesReport(report), inbox)
                .await
            {
                return false;
            }
        }
        for frame in self.replay.frames(&self.shared.outbox).await {
            if !self.send_reading(frame, inbox).await {
                return false;
            }
        }
        let dirty = std::mem::take(&mut self.shared.state().caps_dirty);
        if dirty {
            let capabilities = self.shared.capabilities().await;
            return self
                .send_msg_reading(
                    &RunnerToBroker::CapabilitiesUpdate(CapabilitiesUpdate { capabilities }),
                    inbox,
                )
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
            let last_seq = match d.store.last_seq(&job).await {
                Ok(last_seq) => last_seq,
                Err(e) => {
                    self.shared.diag.event(
                        Level::Error,
                        "received seq unreadable; the job is resumed from the start",
                        &[("job", &job.0), ("error", &e.to_string())],
                    );
                    0
                }
            };
            points.push(ResumePoint { job, last_seq });
        }
        points
    }

    async fn idle_and_stopping(&self) -> bool {
        idle_and_stopping(self.shared).await
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
        match msg {
            BrokerToRunner::Ack(a) => self.on_ack(&a).await,
            BrokerToRunner::Resume(r) => self.on_resume(&r).await,
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

/// Whether the runner was told to stop, runs no job and has no result left to deliver.
pub(crate) async fn idle_and_stopping(s: &Shared) -> bool {
    if !s.state().draining || s.state().job.is_some() {
        return false;
    }
    matches!(s.deps.store.pending_outbox().await, Ok(p) if p.is_empty())
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
        name: s.fixed.name.clone(),
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
                &[("address", &s.deps.broker.address)],
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

const READ_AHEAD: usize = 64;

struct Reader(tokio::task::JoinHandle<()>);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn read_frames(t: Arc<dyn Transport>) -> (Reader, mpsc::Receiver<Frame>) {
    let (tx, rx) = mpsc::channel(READ_AHEAD);
    let task = tokio::spawn(async move {
        while let Ok(frame) = t.recv().await {
            if tx.send(frame).await.is_err() {
                return;
            }
        }
    });
    (Reader(task), rx)
}

pub(crate) async fn session(
    shared: &Arc<Shared>,
    transport: Arc<dyn Transport>,
    slot: &mut JobSlot,
    control: &mut mpsc::Receiver<Control>,
) -> End {
    let t = transport.as_ref();
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
    let (_reader, frames) = read_frames(transport.clone());
    let mut inbox = Inbox {
        frames,
        held: VecDeque::new(),
        heard: false,
    };
    let points = s.resume_points().await;
    let report = shared.services().report(shared.now()).await;
    if !s.send(&RunnerToBroker::Resume(Resume { points })).await
        || !s.send(&RunnerToBroker::ServicesReport(report)).await
        || !s.flush(&mut inbox).await
    {
        return lost();
    }
    let (draining, drain_reason) = {
        let st = shared.state();
        (st.draining, st.drain_reason)
    };
    if draining
        && !s
            .send(&RunnerToBroker::Drain {
                reason: drain_reason,
            })
            .await
    {
        return lost();
    }
    let clock = shared.deps.clock.clone();
    let mut next_beat = after(shared.now(), heartbeat);
    let mut last_rx = shared.now();
    loop {
        if s.idle_and_stopping().await && shared.services_down() {
            if shared.upgrade_ended() {
                shared.resume_after_upgrade();
                t.close().await;
                return lost();
            }
            s.flush(&mut inbox).await;
            shared
                .diag
                .event(Level::Info, "drain finished, leaving", &[]);
            s.send(&RunnerToBroker::Bye(Bye {
                reason: Some("runner stopped".into()),
            }))
            .await;
            t.close().await;
            return End::Stopped;
        }
        let deadline = after(last_rx, silence);
        tokio::select! {
            f = inbox.next() => match f {
                Some(frame) => {
                    last_rx = shared.now();
                    if let Step::End(end) = s.handle(frame).await {
                        return end;
                    }
                }
                None => return lost(),
            },
            _ = shared.wake.notified() => {
                if !s.flush(&mut inbox).await {
                    return lost();
                }
                if inbox.take_heard() {
                    last_rx = shared.now();
                }
            }
            _ = clock.sleep_until(next_beat) => {
                next_beat = after(next_beat, heartbeat);
                if !s
                    .send(&RunnerToBroker::Heartbeat {
                        load: shared.machine_load(),
                    })
                    .await {
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
                    let reason = shared.state().drain_reason;
                    if !s.send(&RunnerToBroker::Drain { reason }).await {
                        return lost();
                    }
                }
            }
        }
    }
}
