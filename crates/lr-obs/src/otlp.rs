//! The `otlp` sink (spec 12.1, 12.2): the only network sink, off by default. Logs and metrics
//! go as OTLP/HTTP JSON to `<endpoint>/v1/logs` and `<endpoint>/v1/metrics` over TLS.
//!
//! Records wait in the sink's bounded queue (drop oldest, counted); the exporter sends them
//! in batches of at most [`BATCH_MAX`]. A failed export loses its batch (counted as dropped)
//! and the exporter backs off, [`BACKOFF_START_MS`] doubling up to [`BACKOFF_MAX_MS`], while
//! the queue keeps its bound, so an unreachable receiver never slows a role. Metrics are sent
//! every [`METRICS_PERIOD_MS`] as cumulative values. All timing reads the [`Clock`].

use crate::config::Signals;
use crate::logger::Resource;
use crate::metrics::Metrics;
use crate::otlp_json::{encode_logs, encode_metrics};
use crate::queue::Writer;
use crate::redact::Redacted;
use lr_abstractions::{Clock, Level};
use lr_core::Timestamp;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Path of the logs signal under the endpoint.
pub const LOGS_PATH: &str = "/v1/logs";
/// Path of the metrics signal under the endpoint.
pub const METRICS_PATH: &str = "/v1/metrics";
/// Metrics are exported this often.
pub const METRICS_PERIOD_MS: u64 = 60_000;
/// First pause after a failed export.
pub const BACKOFF_START_MS: u64 = 5_000;
/// Longest pause after repeated failures.
pub const BACKOFF_MAX_MS: u64 = 300_000;
/// Most log records in one export.
pub const BATCH_MAX: usize = 512;

/// Why an export failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// The receiver answered with a non-2xx HTTP status.
    Status(u16),
    /// Connection, TLS or timeout failure.
    Io(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Status(code) => write!(f, "otlp receiver answered {code}"),
            ExportError::Io(why) => write!(f, "otlp export failed: {why}"),
        }
    }
}

/// Sends one OTLP request body; blocking, bounded by the transport's timeout.
pub trait OtlpTransport: Send + Sync {
    /// POSTs `body` (JSON) to `path` under the endpoint.
    fn post(&self, path: &str, body: &[u8]) -> Result<(), ExportError>;
}

/// OTLP severity number of a level.
pub fn severity_number(level: Level) -> u8 {
    match level {
        Level::Trace => 1,
        Level::Debug => 5,
        Level::Info => 9,
        Level::Warn => 13,
        Level::Error => 17,
    }
}

#[derive(Default)]
struct Sent {
    posts: Vec<(String, Vec<u8>)>,
    attempts: u64,
    failing: bool,
}

/// A transport in memory for tests: keeps successful posts, fails on demand.
#[derive(Default)]
pub struct MemoryTransport {
    sent: Mutex<Sent>,
}

impl MemoryTransport {
    /// A transport that accepts everything.
    pub fn new() -> MemoryTransport {
        MemoryTransport::default()
    }

    /// Successful posts as (path, body).
    pub fn posts(&self) -> Vec<(String, Vec<u8>)> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .posts
            .clone()
    }

    /// Posts tried, failed ones included.
    pub fn attempts(&self) -> u64 {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).attempts
    }

    /// Makes every post fail (or succeed again).
    pub fn set_failing(&self, on: bool) {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).failing = on;
    }
}

impl OtlpTransport for MemoryTransport {
    fn post(&self, path: &str, body: &[u8]) -> Result<(), ExportError> {
        let mut s = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        s.attempts += 1;
        if s.failing {
            return Err(ExportError::Io("receiver down".into()));
        }
        s.posts.push((path.to_string(), body.to_vec()));
        Ok(())
    }
}

/// The exporter of one OTLP sink.
pub struct OtlpWriter {
    transport: Arc<dyn OtlpTransport>,
    clock: Arc<dyn Clock>,
    signals: Signals,
    metrics: Arc<Metrics>,
    resource: Resource,
    next_attempt: Timestamp,
    backoff_ms: u64,
    next_metrics: Timestamp,
    errors: u64,
}

impl OtlpWriter {
    /// An exporter; the first metric export is one period after creation.
    pub fn new(
        transport: Arc<dyn OtlpTransport>,
        clock: Arc<dyn Clock>,
        signals: Signals,
        metrics: Arc<Metrics>,
        resource: Resource,
    ) -> OtlpWriter {
        let now = clock.now();
        OtlpWriter {
            transport,
            clock,
            signals,
            metrics,
            resource,
            next_attempt: now,
            backoff_ms: 0,
            next_metrics: Timestamp(now.0.saturating_add(METRICS_PERIOD_MS)),
            errors: 0,
        }
    }

    fn send(&mut self, path: &str, body: &[u8]) -> bool {
        let result = self.transport.post(path, body);
        let now = self.clock.now();
        match result {
            Ok(()) => {
                self.backoff_ms = 0;
                self.next_attempt = now;
                true
            }
            Err(_) => {
                self.errors = self.errors.saturating_add(1);
                self.backoff_ms = if self.backoff_ms == 0 {
                    BACKOFF_START_MS
                } else {
                    self.backoff_ms.saturating_mul(2).min(BACKOFF_MAX_MS)
                };
                self.next_attempt = Timestamp(now.0.saturating_add(self.backoff_ms));
                false
            }
        }
    }
}

impl Writer for OtlpWriter {
    fn write(&mut self, batch: &[Redacted]) -> u64 {
        if !self.signals.logs || batch.is_empty() {
            return 0;
        }
        let body = encode_logs(&self.resource, batch);
        if self.send(LOGS_PATH, &body) {
            0
        } else {
            batch.len() as u64
        }
    }

    fn ready(&mut self) -> bool {
        self.clock.now() >= self.next_attempt
    }

    fn tick(&mut self) -> u64 {
        let now = self.clock.now();
        if self.signals.metrics && now >= self.next_metrics && self.ready() {
            let body = encode_metrics(&self.resource, &self.metrics, now);
            if self.send(METRICS_PATH, &body) {
                self.next_metrics = Timestamp(now.0.saturating_add(METRICS_PERIOD_MS));
            }
        }
        0
    }

    fn errors(&self) -> u64 {
        self.errors
    }

    fn batch_max(&self) -> usize {
        BATCH_MAX
    }
}
