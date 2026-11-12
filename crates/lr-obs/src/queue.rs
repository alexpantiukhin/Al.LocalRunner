//! The bounded queue in front of every sink (spec 12.1: a sink never blocks the role; the
//! buffer is bounded and on overflow events are dropped and counted).
//!
//! [`Queued::accept`] only enqueues. When the queue is over [`QueueLimits`] the oldest record
//! is dropped, so the newest diagnostics survive a burst; every lost record, from overflow, an
//! I/O failure or a failed export, adds to `localrunner_log_dropped_total{sink}`.

use crate::redact::Redacted;
use lr_abstractions::Meter;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The I/O side of a sink, driven by [`Queued`].
pub trait Writer: Send {
    /// Writes records; returns how many were lost.
    fn write(&mut self, batch: &[Redacted]) -> u64;

    /// Pushes buffered data out; returns records lost doing so.
    fn flush(&mut self) -> u64 {
        0
    }

    /// Whether the writer takes records now (an exporter in back-off does not).
    fn ready(&mut self) -> bool {
        true
    }

    /// Periodic work besides records (metric export); returns records lost.
    fn tick(&mut self) -> u64 {
        0
    }

    /// I/O failures so far.
    fn errors(&self) -> u64 {
        0
    }

    /// Final flush that also releases the writer's resources (closes the file); returns
    /// records lost doing so.
    fn close(&mut self) -> u64 {
        self.flush()
    }

    /// Most records handed to one [`Writer::write`].
    fn batch_max(&self) -> usize {
        256
    }
}

/// Kind of a sink, the `sink` label of `localrunner_log_dropped_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// `file`.
    File,
    /// `console`.
    Console,
    /// `journal`.
    Journal,
    /// `otlp`.
    Otlp,
}

impl SinkKind {
    /// The label value.
    pub fn label(self) -> &'static str {
        match self {
            SinkKind::File => "file",
            SinkKind::Console => "console",
            SinkKind::Journal => "journal",
            SinkKind::Otlp => "otlp",
        }
    }
}

/// Longest sleep of a sink worker; also the period of its flush and periodic work.
pub const WORKER_PERIOD: Duration = Duration::from_secs(1);

/// Bounds of one sink's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    /// Most records waiting.
    pub max_records: usize,
    /// Most bytes waiting (by [`Redacted::size`]).
    pub max_bytes: usize,
}

impl Default for QueueLimits {
    fn default() -> QueueLimits {
        QueueLimits {
            max_records: 10_000,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

struct Pending {
    items: VecDeque<Redacted>,
    bytes: usize,
    closed: bool,
}

/// A sink: a bounded queue plus its [`Writer`].
pub struct Queued {
    kind: SinkKind,
    limits: QueueLimits,
    pending: Mutex<Pending>,
    wake: Condvar,
    writer: Mutex<Box<dyn Writer>>,
    finished: AtomicBool,
    dropped: AtomicU64,
    errors: AtomicU64,
    meter: Arc<dyn Meter>,
}

impl Queued {
    /// A sink of `kind` writing through `writer`.
    pub fn new(
        kind: SinkKind,
        writer: Box<dyn Writer>,
        limits: QueueLimits,
        meter: Arc<dyn Meter>,
    ) -> Queued {
        Queued {
            kind,
            limits,
            pending: Mutex::new(Pending {
                items: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            writer: Mutex::new(writer),
            finished: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            meter,
        }
    }

    /// The sink's kind.
    pub fn kind(&self) -> SinkKind {
        self.kind
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lose(&self, n: u64) {
        if n > 0 {
            self.dropped.fetch_add(n, Ordering::Relaxed);
            self.meter.add(
                "localrunner_log_dropped_total",
                &[("sink", self.kind.label())],
                n,
            );
        }
    }

    /// Enqueues a record without blocking; drops the oldest records beyond the limits.
    pub fn accept(&self, record: Redacted) {
        let mut lost = 0u64;
        {
            let mut p = self.pending();
            if p.closed {
                lost = 1;
            } else {
                p.bytes = p.bytes.saturating_add(record.size());
                p.items.push_back(record);
                while !p.items.is_empty()
                    && (p.items.len() > self.limits.max_records || p.bytes > self.limits.max_bytes)
                {
                    if let Some(old) = p.items.pop_front() {
                        p.bytes = p.bytes.saturating_sub(old.size());
                        lost += 1;
                    }
                }
            }
        }
        self.lose(lost);
        self.wake.notify_one();
    }

    fn take(&self, max: usize) -> Vec<Redacted> {
        let mut p = self.pending();
        let n = p.items.len().min(max);
        let batch: Vec<Redacted> = p.items.drain(..n).collect();
        let bytes: usize = batch.iter().map(Redacted::size).sum();
        p.bytes = p.bytes.saturating_sub(bytes);
        batch
    }

    /// Writes what is queued while the writer is ready and does the periodic work; returns
    /// whether the writer was still ready at the end (`false` while an exporter backs off).
    pub fn pump(&self) -> bool {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if self.finished.load(Ordering::SeqCst) {
            return false;
        }
        let ready = loop {
            if !w.ready() {
                break false;
            }
            let batch = self.take(w.batch_max());
            if batch.is_empty() {
                break true;
            }
            let lost = w.write(&batch);
            self.lose(lost);
        };
        let lost = w.tick();
        self.lose(lost);
        self.errors.store(w.errors(), Ordering::Relaxed);
        ready
    }

    /// [`Queued::pump`], then pushes buffered data out; returns what `pump` returned.
    pub fn flush(&self) -> bool {
        let ready = self.pump();
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if self.finished.load(Ordering::SeqCst) {
            return false;
        }
        let lost = w.flush();
        self.lose(lost);
        self.errors.store(w.errors(), Ordering::Relaxed);
        ready
    }

    /// Stops taking records; later ones are dropped and counted. Wakes a waiting worker.
    pub fn close(&self) {
        self.pending().closed = true;
        self.wake.notify_all();
    }

    /// Whether [`Queued::close`] was called.
    pub fn is_closed(&self) -> bool {
        self.pending().closed
    }

    /// Final delivery after [`Queued::close`]: flushes, closes the writer and counts what
    /// still could not be delivered as dropped. From then on the writer is never used again:
    /// a later `pump`, `flush` or `finish` (from a thread still holding the queue) does
    /// nothing, so a retired file sink cannot reopen its file.
    pub fn finish(&self) {
        self.pump();
        {
            let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            if self.finished.swap(true, Ordering::SeqCst) {
                return;
            }
            let lost = w.close();
            self.lose(lost);
            self.errors.store(w.errors(), Ordering::Relaxed);
        }
        let left = {
            let mut p = self.pending();
            let n = p.items.len() as u64;
            p.items.clear();
            p.bytes = 0;
            n
        };
        self.lose(left);
    }

    /// Records lost so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// I/O failures of the writer so far.
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// The worker loop of the sink: delivers until [`Queued::close`]. While the writer is not
    /// ready (an exporter backing off) the worker sleeps a whole [`WORKER_PERIOD`] and ignores
    /// arriving records, so a paused sink costs a few wake-ups per second, not a busy loop.
    pub fn run(&self) {
        let mut ready = true;
        while !self.is_closed() {
            self.wait(WORKER_PERIOD, ready);
            ready = self.flush();
        }
    }

    fn wait(&self, timeout: Duration, ready: bool) {
        let p = self.pending();
        let _ = self
            .wake
            .wait_timeout_while(p, timeout, |p| !p.closed && !(ready && !p.items.is_empty()));
    }
}
