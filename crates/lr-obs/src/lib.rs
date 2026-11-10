//! Diagnostics of the system itself (spec 12): log sinks (file, console, OS journal, OTLP),
//! field redaction and metrics.
//!
//! Roles never depend on this crate. They write [`lr_abstractions::Record`]s to an
//! `Arc<dyn LogSink>` and metric updates to an `Arc<dyn Meter>`; `lr-cli` builds a [`Logger`]
//! and a [`Metrics`] from `logging.toml` and hands them over. Every record passes the
//! [`Redactor`] before any sink sees it: sinks only accept a [`redact::Redacted`] record,
//! which nothing outside the redactor can construct.
//!
//! [`Logger::write`] never blocks: each sink has a bounded queue that drops its oldest record
//! when full and counts the loss in `localrunner_log_dropped_total{sink}`; the I/O happens on
//! the sink's worker thread ([`Logger::start_workers`]) or in [`Logger::flush`].

pub mod config;
pub mod console;
pub mod file;
pub mod format;
pub mod fs;
pub mod https;
pub mod logger;
pub mod metrics;
pub mod oslog;
pub mod otlp;
pub mod otlp_json;
pub mod queue;
pub mod redact;
mod scan;
mod time;

pub use config::{ConfigError, LoggingConfig};
pub use logger::{Logger, ObsEnv};
pub use lr_abstractions::MASK;
pub use metrics::Metrics;
pub use redact::{RedactingSecrets, Redactor};
