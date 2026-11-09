use lr_core::Timestamp;

/// Severity of a diagnostic event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Very detailed tracing.
    Trace,
    /// Debugging detail.
    Debug,
    /// Normal operation.
    Info,
    /// Something unexpected that the role survived.
    Warn,
    /// A failure.
    Error,
}

/// Role of the process that wrote an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Client side.
    Hub,
    /// Pool broker.
    Broker,
    /// Runner.
    Runner,
}

/// One structured diagnostic event (spec 12): time, level, role, component, MachineId, message
/// and contextual fields such as `job_id`, runner, client label or the fired rule. Job output,
/// commands, environment, keys, join codes and tokens never belong here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// When it happened.
    pub time: Timestamp,
    /// Severity.
    pub level: Level,
    /// Role of the writer.
    pub role: Role,
    /// Component inside the role (`engine`, `outbox`, `policy`, ...).
    pub component: String,
    /// MachineId of the installation.
    pub machine_id: String,
    /// Human-readable message.
    pub message: String,
    /// Contextual fields in insertion order.
    pub fields: Vec<(String, String)>,
}

impl Record {
    /// An event without MachineId and fields.
    pub fn new(
        time: Timestamp,
        level: Level,
        role: Role,
        component: &str,
        message: &str,
    ) -> Record {
        Record {
            time,
            level,
            role,
            component: component.to_string(),
            machine_id: String::new(),
            message: message.to_string(),
            fields: Vec::new(),
        }
    }

    /// Adds a contextual field.
    pub fn field(mut self, name: &str, value: &str) -> Record {
        self.fields.push((name.to_string(), value.to_string()));
        self
    }
}

/// A receiver of diagnostic events (file, console, OS journal, OTLP; implemented by `lr-obs`).
///
/// `write` never blocks and never fails visibly: a sink buffers within a bound and, when the
/// buffer is full or the receiver is down, drops the event and counts it, so an unavailable
/// receiver never stops or slows a role (spec 12.1). Field redaction is the sink's job.
pub trait LogSink: Send + Sync {
    /// Takes one event without blocking.
    fn write(&self, record: &Record);

    /// Events dropped so far (`log_dropped_total`).
    fn dropped(&self) -> u64;
}
