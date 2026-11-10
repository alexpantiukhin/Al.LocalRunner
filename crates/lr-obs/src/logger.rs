//! The [`Logger`]: the `LogSink` handed to roles. It filters by level per component, redacts
//! once and enqueues the redacted record to every sink; the sinks do their I/O on worker
//! threads or in [`Logger::flush`].

use crate::config::{parse_endpoint, ConfigError, Endpoint, LoggingConfig, SinkConfig};
use crate::console::ConsoleWriter;
use crate::file::{FilePolicy, FileWriter};
use crate::fs::{LogFs, OsLogFs};
use crate::https::HttpsTransport;
use crate::metrics::Metrics;
use crate::oslog::{OsLogApi, OsLogWriter};
use crate::otlp::{OtlpTransport, OtlpWriter};
use crate::queue::{QueueLimits, Queued, SinkKind, Writer};
use crate::redact::Redactor;
use lr_abstractions::{Clock, LogSink, Meter, Record, Role};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// A shared console writer (stderr in production).
pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

/// Builds the transport of an enabled OTLP sink.
pub type OtlpFactory =
    Arc<dyn Fn(&OtlpSettings) -> Result<Arc<dyn OtlpTransport>, ConfigError> + Send + Sync>;

/// Resolved settings of an enabled OTLP sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpSettings {
    /// Receiver.
    pub endpoint: Endpoint,
    /// CA certificates file, resolved against the state directory.
    pub ca_file: PathBuf,
    /// Limit of one connection step.
    pub timeout: Duration,
}

/// Who writes: exported as OTLP resource attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// Role of the process.
    pub role: Role,
    /// MachineId of the installation.
    pub machine_id: String,
}

/// What the logger needs from its process.
pub struct ObsEnv {
    /// State directory of the role; file sink paths are relative to it.
    pub state_dir: PathBuf,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// File system of the file sinks.
    pub fs: Arc<dyn LogFs>,
    /// Console writer, `Some` only when the process runs in the foreground.
    pub console: Option<SharedWriter>,
    /// The OS journal, `None` where there is none.
    pub os_log: Option<Arc<dyn OsLogApi>>,
    /// Transport factory of OTLP sinks.
    pub otlp: OtlpFactory,
    /// Metric registry of the process.
    pub metrics: Arc<Metrics>,
    /// Resource attributes.
    pub resource: Resource,
    /// Bounds of each sink's queue.
    pub limits: QueueLimits,
}

/// The real OTLP transport: reads `ca_file` and connects over verified TLS.
pub fn https_factory() -> OtlpFactory {
    Arc::new(|settings: &OtlpSettings| {
        let ca = std::fs::read(&settings.ca_file)
            .map_err(|e| ConfigError(format!("[[sink]] otlp: cannot read ca_file: {e}")))?;
        let transport = HttpsTransport::new(&settings.endpoint, &ca, settings.timeout)?;
        Ok(Arc::new(transport) as Arc<dyn OtlpTransport>)
    })
}

impl ObsEnv {
    /// The production environment: real files, stderr when `foreground`, the OS journal and
    /// the HTTPS transport.
    pub fn system(
        state_dir: PathBuf,
        clock: Arc<dyn Clock>,
        metrics: Arc<Metrics>,
        resource: Resource,
        foreground: bool,
    ) -> ObsEnv {
        let console: Option<SharedWriter> =
            foreground.then(|| Arc::new(Mutex::new(std::io::stderr())) as SharedWriter);
        ObsEnv {
            state_dir,
            clock,
            fs: Arc::new(OsLogFs),
            console,
            os_log: crate::oslog::system(),
            otlp: https_factory(),
            metrics,
            resource,
            limits: QueueLimits::default(),
        }
    }
}

struct Sink {
    queue: Arc<Queued>,
    logs: bool,
}

struct Pipeline {
    config: LoggingConfig,
    sinks: Vec<Sink>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

/// The diagnostic sink of a process (spec 12): level filter, redaction, then every sink.
pub struct Logger {
    env: ObsEnv,
    redactor: Arc<Redactor>,
    current: RwLock<Arc<Pipeline>>,
    workers_on: AtomicBool,
    stopped: AtomicBool,
    retired_dropped: AtomicU64,
    retired_errors: AtomicU64,
}

impl Logger {
    /// A logger for `config`; no thread is started (see [`Logger::start_workers`]).
    pub fn new(config: &LoggingConfig, env: ObsEnv) -> Result<Logger, ConfigError> {
        let pipeline = build(config, &env)?;
        env.metrics.set_client_label(config.client_label);
        Ok(Logger {
            redactor: Arc::new(Redactor::new(&config.redact_fields)),
            current: RwLock::new(Arc::new(pipeline)),
            env,
            workers_on: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            retired_dropped: AtomicU64::new(0),
            retired_errors: AtomicU64::new(0),
        })
    }

    /// The redactor, to register secret values ([`Redactor::protect`]).
    pub fn redactor(&self) -> Arc<Redactor> {
        self.redactor.clone()
    }

    fn pipeline(&self) -> Arc<Pipeline> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Applies a new configuration (`logging reload`). On error the old one stays.
    pub fn reload(&self, config: &LoggingConfig) -> Result<(), ConfigError> {
        let fresh = Arc::new(build(config, &self.env)?);
        self.redactor.set_fields(&config.redact_fields);
        self.env.metrics.set_client_label(config.client_label);
        let old = std::mem::replace(
            &mut *self.current.write().unwrap_or_else(|e| e.into_inner()),
            fresh.clone(),
        );
        if self.workers_on.load(Ordering::SeqCst) {
            spawn(&fresh);
        }
        self.retire(&old);
        Ok(())
    }

    fn retire(&self, p: &Pipeline) {
        stop(p);
        for sink in &p.sinks {
            self.retired_dropped
                .fetch_add(sink.queue.dropped(), Ordering::Relaxed);
            self.retired_errors
                .fetch_add(sink.queue.errors(), Ordering::Relaxed);
        }
    }

    /// Delivers everything queued now, on the calling thread.
    pub fn flush(&self) {
        for sink in &self.pipeline().sinks {
            sink.queue.flush();
        }
    }

    /// Starts one worker thread per sink, also for sinks of later reloads.
    pub fn start_workers(&self) {
        if self.stopped.load(Ordering::SeqCst) || self.workers_on.swap(true, Ordering::SeqCst) {
            return;
        }
        spawn(&self.pipeline());
    }

    /// Stops the workers and delivers what is queued; records written later are dropped
    /// and counted.
    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        stop(&self.pipeline());
    }

    /// I/O and export failures of all sinks so far.
    pub fn errors(&self) -> u64 {
        let live: u64 = self.pipeline().sinks.iter().map(|s| s.queue.errors()).sum();
        live.saturating_add(self.retired_errors.load(Ordering::Relaxed))
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl LogSink for Logger {
    fn write(&self, record: &Record) {
        let p = self.pipeline();
        if record.level < p.config.level_for(&record.component) {
            return;
        }
        let redacted = self.redactor.redact(record);
        for sink in p.sinks.iter().filter(|s| s.logs) {
            sink.queue.accept(redacted.clone());
        }
    }

    fn dropped(&self) -> u64 {
        let live: u64 = self
            .pipeline()
            .sinks
            .iter()
            .map(|s| s.queue.dropped())
            .sum();
        live.saturating_add(self.retired_dropped.load(Ordering::Relaxed))
    }
}

fn spawn(p: &Pipeline) {
    let mut workers = p.workers.lock().unwrap_or_else(|e| e.into_inner());
    for sink in &p.sinks {
        let queue = sink.queue.clone();
        let name = format!("lr-obs-{}", queue.kind().label());
        let started = std::thread::Builder::new().name(name).spawn(move || {
            while !queue.is_closed() {
                queue.wait(Duration::from_secs(1));
                queue.flush();
            }
        });
        if let Ok(handle) = started {
            workers.push(handle);
        }
    }
}

fn stop(p: &Pipeline) {
    for sink in &p.sinks {
        sink.queue.close();
    }
    let handles: Vec<JoinHandle<()>> =
        std::mem::take(&mut *p.workers.lock().unwrap_or_else(|e| e.into_inner()));
    for handle in handles {
        let _ = handle.join();
    }
    for sink in &p.sinks {
        sink.queue.finish();
    }
}

fn build(config: &LoggingConfig, env: &ObsEnv) -> Result<Pipeline, ConfigError> {
    let meter: Arc<dyn Meter> = env.metrics.clone();
    let mut sinks = Vec::new();
    let mut add = |kind: SinkKind, writer: Box<dyn Writer>, logs: bool| {
        let queue = Arc::new(Queued::new(kind, writer, env.limits, meter.clone()));
        sinks.push(Sink { queue, logs });
    };
    for sink in &config.sinks {
        match sink {
            SinkConfig::File(f) => {
                let policy = FilePolicy {
                    path: env.state_dir.join(&f.path),
                    format: f.format,
                    rotate_bytes: f.rotate_mb.saturating_mul(1024 * 1024),
                    keep_days: f.keep_days,
                    keep_files: f.keep_files,
                };
                let writer = FileWriter::new(policy, env.fs.clone(), env.clock.clone());
                add(SinkKind::File, Box::new(writer), true);
            }
            SinkConfig::Console => {
                if let Some(out) = &env.console {
                    add(
                        SinkKind::Console,
                        Box::new(ConsoleWriter::new(out.clone())),
                        true,
                    );
                }
            }
            SinkConfig::Journal => {
                let api = env.os_log.clone().ok_or_else(|| {
                    ConfigError("[[sink]] journal: this system has no OS journal".into())
                })?;
                add(SinkKind::Journal, Box::new(OsLogWriter::new(api)), true);
            }
            SinkConfig::Otlp(o) if o.enabled => {
                let ca = o.ca_file.clone().ok_or_else(|| {
                    ConfigError("[[sink]] otlp: an enabled sink needs ca_file".into())
                })?;
                let settings = OtlpSettings {
                    endpoint: parse_endpoint(&o.endpoint)?,
                    ca_file: env.state_dir.join(ca),
                    timeout: Duration::from_secs(o.timeout_s),
                };
                let transport = (env.otlp)(&settings)?;
                let writer = OtlpWriter::new(
                    transport,
                    env.clock.clone(),
                    o.signals,
                    env.metrics.clone(),
                    env.resource.clone(),
                );
                add(SinkKind::Otlp, Box::new(writer), o.signals.logs);
            }
            SinkConfig::Otlp(_) => {}
        }
    }
    Ok(Pipeline {
        config: config.clone(),
        sinks,
        workers: Mutex::new(Vec::new()),
    })
}
