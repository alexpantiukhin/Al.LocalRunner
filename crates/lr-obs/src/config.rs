//! `logging.toml` of a role's state directory (spec 12.1). Without the file, and when it lists
//! no `[[sink]]`, a rotated JSON file `logs/daemon.log` at level `info` is used.

use crate::format::Format;
use lr_abstractions::Level;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Why `logging.toml` was refused. The text never repeats endpoint values, which may carry
/// credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "logging.toml: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

fn bad(text: impl Into<String>) -> ConfigError {
    ConfigError(text.into())
}

/// Default `path` of a file sink, relative to the state directory.
pub const DEFAULT_FILE_PATH: &str = "logs/daemon.log";
/// Default `rotate_mb`: the active file is rotated before it would exceed this size.
pub const DEFAULT_ROTATE_MB: u64 = 50;
/// Default `keep_days`: rotated files older than this are deleted.
pub const DEFAULT_KEEP_DAYS: u32 = 14;
/// Default `keep_files`: at most this many rotated files are kept.
pub const DEFAULT_KEEP_FILES: u32 = 10;
/// Default `timeout_s` of one OTLP export.
pub const DEFAULT_OTLP_TIMEOUT_S: u64 = 10;

/// Which signals an OTLP sink sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals {
    /// Diagnostic events.
    pub logs: bool,
    /// Metrics of spec 12.2.
    pub metrics: bool,
}

/// A `kind = "file"` sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSinkConfig {
    /// Path relative to the state directory; never leaves it.
    pub path: PathBuf,
    /// Line format.
    pub format: Format,
    /// Size limit of the active file in MiB.
    pub rotate_mb: u64,
    /// Age in days after which a rotated file is deleted.
    pub keep_days: u32,
    /// Most rotated files kept.
    pub keep_files: u32,
}

/// A `kind = "otlp"` sink: the only network sink, TLS only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpSinkConfig {
    /// `https://host[:port][/path]` of an OTLP/HTTP receiver.
    pub endpoint: String,
    /// Signals sent.
    pub signals: Signals,
    /// Off by default; nothing leaves the machine until the owner turns it on.
    pub enabled: bool,
    /// PEM file with the CA certificates the receiver's certificate must chain to; relative
    /// paths are resolved against the state directory. Required when enabled.
    pub ca_file: Option<PathBuf>,
    /// Limit of one export, connection included.
    pub timeout_s: u64,
}

/// One `[[sink]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkConfig {
    /// JSON lines file with rotation.
    File(FileSinkConfig),
    /// stderr, only while the process runs in the foreground.
    Console,
    /// journald/syslog or the Windows Event Log.
    Journal,
    /// OpenTelemetry receiver.
    Otlp(OtlpSinkConfig),
}

/// The whole `logging.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    /// `[logging] level`.
    pub level: Level,
    /// `[logging.levels]`: level per component.
    pub levels: BTreeMap<String, Level>,
    /// `[[sink]]` entries.
    pub sinks: Vec<SinkConfig>,
    /// `[redact] fields`, added to the built-in sensitive names.
    pub redact_fields: Vec<String>,
    /// `[metrics] client_label`: allows the `client` label (spec 12.2), off by default.
    pub client_label: bool,
}

fn default_file() -> FileSinkConfig {
    FileSinkConfig {
        path: PathBuf::from(DEFAULT_FILE_PATH),
        format: Format::Json,
        rotate_mb: DEFAULT_ROTATE_MB,
        keep_days: DEFAULT_KEEP_DAYS,
        keep_files: DEFAULT_KEEP_FILES,
    }
}

impl Default for LoggingConfig {
    fn default() -> LoggingConfig {
        LoggingConfig {
            level: Level::Info,
            levels: BTreeMap::new(),
            sinks: vec![SinkConfig::File(default_file())],
            redact_fields: Vec::new(),
            client_label: false,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLogging {
    level: Option<String>,
    #[serde(default)]
    levels: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRedact {
    #[serde(default)]
    fields: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMetrics {
    #[serde(default)]
    client_label: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    logging: RawLogging,
    #[serde(default)]
    sink: Vec<toml::Table>,
    #[serde(default)]
    redact: RawRedact,
    #[serde(default)]
    metrics: RawMetrics,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    path: Option<PathBuf>,
    format: Option<Format>,
    rotate_mb: Option<u64>,
    keep_days: Option<u32>,
    keep_files: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmpty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOtlp {
    endpoint: String,
    signals: Option<Vec<String>>,
    #[serde(default)]
    enabled: bool,
    ca_file: Option<PathBuf>,
    timeout_s: Option<u64>,
}

fn toml_error(e: toml::de::Error) -> ConfigError {
    match e.span() {
        Some(span) => bad(format!("{} (at byte {})", e.message(), span.start)),
        None => bad(e.message().to_string()),
    }
}

fn level(name: &str) -> Result<Level, ConfigError> {
    match name {
        "trace" => Ok(Level::Trace),
        "debug" => Ok(Level::Debug),
        "info" => Ok(Level::Info),
        "warn" => Ok(Level::Warn),
        "error" => Ok(Level::Error),
        other => Err(bad(format!(
            "unknown level `{other}`, expected trace, debug, info, warn or error"
        ))),
    }
}

fn table_into<T: DeserializeOwned>(table: toml::Table, kind: &str) -> Result<T, ConfigError> {
    toml::Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| bad(format!("[[sink]] {kind}: {}", e.message())))
}

fn positive<T: PartialOrd + Default>(value: T, key: &str) -> Result<T, ConfigError> {
    if value > T::default() {
        Ok(value)
    } else {
        Err(bad(format!("[[sink]] file: {key} must be at least 1")))
    }
}

fn file_sink(raw: RawFile) -> Result<SinkConfig, ConfigError> {
    let d = default_file();
    let path = raw.path.unwrap_or(d.path);
    check_relative_path(&path)?;
    Ok(SinkConfig::File(FileSinkConfig {
        path,
        format: raw.format.unwrap_or(d.format),
        rotate_mb: positive(raw.rotate_mb.unwrap_or(d.rotate_mb), "rotate_mb")?,
        keep_days: positive(raw.keep_days.unwrap_or(d.keep_days), "keep_days")?,
        keep_files: positive(raw.keep_files.unwrap_or(d.keep_files), "keep_files")?,
    }))
}

fn signals(list: Option<Vec<String>>) -> Result<Signals, ConfigError> {
    let Some(list) = list else {
        return Ok(Signals {
            logs: true,
            metrics: true,
        });
    };
    let mut s = Signals {
        logs: false,
        metrics: false,
    };
    for name in &list {
        match name.as_str() {
            "logs" => s.logs = true,
            "metrics" => s.metrics = true,
            other => {
                return Err(bad(format!(
                    "[[sink]] otlp: unknown signal `{other}`, expected logs or metrics"
                )))
            }
        }
    }
    if list.is_empty() {
        return Err(bad("[[sink]] otlp: signals must not be empty"));
    }
    Ok(s)
}

fn otlp_sink(raw: RawOtlp) -> Result<SinkConfig, ConfigError> {
    parse_endpoint(&raw.endpoint)?;
    let timeout_s = raw.timeout_s.unwrap_or(DEFAULT_OTLP_TIMEOUT_S);
    if !(1..=300).contains(&timeout_s) {
        return Err(bad("[[sink]] otlp: timeout_s must be between 1 and 300"));
    }
    if raw.enabled && raw.ca_file.is_none() {
        return Err(bad("[[sink]] otlp: an enabled sink needs ca_file"));
    }
    Ok(SinkConfig::Otlp(OtlpSinkConfig {
        endpoint: raw.endpoint,
        signals: signals(raw.signals)?,
        enabled: raw.enabled,
        ca_file: raw.ca_file,
        timeout_s,
    }))
}

fn sink(mut table: toml::Table) -> Result<SinkConfig, ConfigError> {
    let kind = match table.remove("kind") {
        Some(toml::Value::String(k)) => k,
        Some(_) => return Err(bad("[[sink]]: kind must be a string")),
        None => return Err(bad("[[sink]]: kind is required")),
    };
    match kind.as_str() {
        "file" => file_sink(table_into(table, "file")?),
        "console" => table_into::<RawEmpty>(table, "console").map(|_| SinkConfig::Console),
        "journal" => table_into::<RawEmpty>(table, "journal").map(|_| SinkConfig::Journal),
        "otlp" => otlp_sink(table_into(table, "otlp")?),
        other => Err(bad(format!(
            "[[sink]]: unknown kind `{other}`, expected file, console, journal or otlp"
        ))),
    }
}

impl LoggingConfig {
    /// Reads `logging.toml`; unknown keys, kinds, levels and signals are refused.
    pub fn parse(text: &str) -> Result<LoggingConfig, ConfigError> {
        let raw: Raw = toml::from_str(text).map_err(toml_error)?;
        let mut levels = BTreeMap::new();
        for (component, name) in &raw.logging.levels {
            levels.insert(component.clone(), level(name)?);
        }
        let mut sinks = raw
            .sink
            .into_iter()
            .map(sink)
            .collect::<Result<Vec<_>, _>>()?;
        if sinks.is_empty() {
            sinks.push(SinkConfig::File(default_file()));
        }
        Ok(LoggingConfig {
            level: raw
                .logging
                .level
                .as_deref()
                .map(level)
                .transpose()?
                .unwrap_or(Level::Info),
            levels,
            sinks,
            redact_fields: raw.redact.fields,
            client_label: raw.metrics.client_label,
        })
    }

    /// The configuration from the file's text, or the default without a file.
    pub fn load(text: Option<&str>) -> Result<LoggingConfig, ConfigError> {
        text.map_or_else(|| Ok(LoggingConfig::default()), LoggingConfig::parse)
    }

    /// The lowest level written for `component`.
    pub fn level_for(&self, component: &str) -> Level {
        self.levels.get(component).copied().unwrap_or(self.level)
    }
}

const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL"];

fn reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    RESERVED.contains(&base.as_str())
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes()[3].is_ascii_digit())
}

/// Checks that a file sink path stays inside the state directory and is a valid file name on
/// every supported OS: relative, no `..`, no drive or root, no reserved device names, no
/// `<>:"|?*\`, control characters or trailing dot or space.
pub fn check_relative_path(path: &Path) -> Result<(), ConfigError> {
    let refuse = |why: &str| Err(bad(format!("[[sink]] file: path {why}")));
    let text = path.to_string_lossy();
    if text.is_empty() || text.ends_with(['/', '\\']) {
        return refuse("must name a file");
    }
    let mut last_normal = false;
    for component in path.components() {
        last_normal = false;
        match component {
            Component::CurDir => {}
            Component::ParentDir => return refuse("must not contain `..`"),
            Component::Prefix(_) | Component::RootDir => return refuse("must be relative"),
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return refuse("must be valid Unicode");
                };
                if part
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*\\".contains(c))
                {
                    return refuse("contains a character not allowed in file names");
                }
                if part.ends_with(['.', ' ']) || reserved(part) {
                    return refuse("uses a name not allowed on Windows");
                }
                last_normal = true;
            }
        }
    }
    if last_normal {
        Ok(())
    } else {
        refuse("must name a file")
    }
}

/// Where an OTLP sink sends: `https://host[:port][/path]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Host name or IP address (IPv6 without brackets).
    pub host: String,
    /// Port, 443 when not given.
    pub port: u16,
    /// Path prefix without the trailing slash, empty for none.
    pub path: String,
}

/// Parses an OTLP endpoint: `https` only, no credentials, query or fragment. Errors never
/// repeat the text.
pub fn parse_endpoint(text: &str) -> Result<Endpoint, ConfigError> {
    let refuse = |why: &str| Err(bad(format!("[[sink]] otlp: endpoint {why}")));
    let Some(rest) = text.strip_prefix("https://") else {
        return refuse("must start with https://");
    };
    if rest.contains('@') {
        return refuse("must not contain credentials");
    }
    if rest.contains(['?', '#']) {
        return refuse("must not contain a query or fragment");
    }
    if rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return refuse("must not contain spaces");
    }
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let Some((host, after)) = v6.split_once(']') else {
            return refuse("has an unclosed IPv6 bracket");
        };
        if !host
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return refuse("has an invalid IPv6 address");
        }
        if !after.is_empty() && !after.starts_with(':') {
            return refuse("has text after the IPv6 address");
        }
        (host, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
    {
        return refuse("has an invalid host");
    }
    let port = match port {
        None => 443,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return refuse("has an invalid port"),
        },
    };
    Ok(Endpoint {
        host: host.to_string(),
        port,
        path: path.trim_end_matches('/').to_string(),
    })
}
