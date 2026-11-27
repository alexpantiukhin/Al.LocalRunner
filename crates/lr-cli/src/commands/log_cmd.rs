//! `log <job> [--step] [--tail] [-f]` (spec 8, 11.1): the log a broker stores, relayed by the
//! hub in order; `-f` keeps following a running job until it ends. The broker refuses logs its
//! visibility policy does not give this client, and every line passes the secret redactor
//! before it is printed.

use super::jobs::hub;
use super::view::{problem, reachable};
use super::LogArgs;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_core::JobId;
use lr_obs::{LoggingConfig, Redactor};
use lr_proto::{CliRead, CliReply, CliRequest, ReadReply, ReadRequest};
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

const MAX_PENDING: usize = 64 * 1024;

/// Turns log text into redacted output: only whole lines pass, so a secret split across two
/// chunks is still masked; a line longer than 64 KiB passes in parts.
pub struct LogPrinter {
    redactor: Arc<Redactor>,
    pending: String,
    named: bool,
}

/// The redactor of CLI output: the built-in sensitive names plus `[redact] fields` of
/// `logging.toml` of the installation (an unreadable file leaves the built-in names).
pub fn cli_redactor(layout: &Layout) -> Arc<Redactor> {
    let fields = std::fs::read_to_string(layout.config_file("logging.toml"))
        .ok()
        .and_then(|t| LoggingConfig::parse(&t).ok())
        .map(|c| c.redact_fields)
        .unwrap_or_default();
    Arc::new(Redactor::new(&fields))
}

impl Default for LogPrinter {
    fn default() -> LogPrinter {
        LogPrinter::new(Arc::new(Redactor::new(&[])))
    }
}

impl LogPrinter {
    /// A printer that masks with `redactor` and names the rule of a value masked by a sensitive
    /// name (`***(name: command)`).
    pub fn new(redactor: Arc<Redactor>) -> LogPrinter {
        LogPrinter {
            redactor,
            pending: String::new(),
            named: true,
        }
    }

    /// A printer that masks with `redactor` and shows every mask as `***`.
    pub fn plain(redactor: Arc<Redactor>) -> LogPrinter {
        LogPrinter {
            named: false,
            ..LogPrinter::new(redactor)
        }
    }

    fn fresh(&self) -> LogPrinter {
        LogPrinter {
            redactor: self.redactor.clone(),
            pending: String::new(),
            named: self.named,
        }
    }

    fn redact(&self, text: &str) -> String {
        match self.named {
            true => self.redactor.redact_job_text(text),
            false => self.redactor.redact_text(text),
        }
    }

    /// The redacted whole lines that `text` completes.
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let cut = match self.pending.rfind('\n') {
            Some(i) => i + 1,
            None if self.pending.len() > MAX_PENDING => self.pending.len(),
            None => return String::new(),
        };
        let done: String = self.pending.drain(..cut).collect();
        self.redact(&done)
    }

    /// The redacted rest after the last line end.
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        self.redact(&rest)
    }
}

/// What `log` asks for.
#[derive(Debug, Clone)]
pub struct LogQuery {
    /// The job.
    pub job: JobId,
    /// Only this step.
    pub step: Option<String>,
    /// Only the last lines.
    pub tail: Option<u64>,
    /// Follow a running job until it ends.
    pub follow: bool,
}

/// Reads the log of `q.job` from the first broker that has it and hands the redacted text to
/// `out` as it comes; the result is the broker's name. A refusal of the broker ends it.
pub async fn find_log(
    client: &IpcClient,
    broker: Option<&str>,
    q: &LogQuery,
    redactor: &Arc<Redactor>,
    out: &mut dyn FnMut(&str),
) -> Result<String, String> {
    find_log_with(client, broker, q, LogPrinter::new(redactor.clone()), out).await
}

/// [`find_log`] with the printer that masks the text.
pub async fn find_log_with(
    client: &IpcClient,
    broker: Option<&str>,
    q: &LogQuery,
    printer: LogPrinter,
    out: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let (names, mut problems) = reachable(client, broker).await?;
    let mut found: Vec<(String, String)> = Vec::new();
    for b in names {
        let req = CliRequest::Read(CliRead {
            broker: Some(b.clone()),
            request: ReadRequest::Log {
                job: q.job.clone(),
                step: q.step.clone(),
                tail: q.tail,
                follow: q.follow,
                client: None,
            },
        });
        client.send(&req).await?;
        let mut printer = printer.fresh();
        let mut held = String::new();
        loop {
            match client.recv::<CliReply>().await? {
                CliReply::LogChunk { text, end, .. } => {
                    let shown = printer.push(&text);
                    if q.follow {
                        out(&shown);
                    } else {
                        held.push_str(&shown);
                    }
                    if end {
                        let rest = printer.finish();
                        if q.follow {
                            out(&rest);
                            return Ok(b);
                        }
                        held.push_str(&rest);
                        found.push((b, held));
                        break;
                    }
                }
                CliReply::Read {
                    reply: ReadReply::NotFound { .. },
                    ..
                } => break,
                CliReply::Read { reply, .. } => return Err(problem(&b, &reply)),
                CliReply::Error(e) => {
                    out(&printer.finish());
                    return Err(format!("broker {b}: {}", e.message));
                }
                _ => continue,
            }
        }
    }
    if let Some((b, text)) = choose_log(found) {
        out(&text);
        return Ok(b);
    }
    problems.insert(
        0,
        format!("job {} is not found on the brokers of this hub", q.job),
    );
    Err(problems.join("\n"))
}

/// The log to show when several brokers know the job: a moved order is known to the broker
/// that gave it back (an empty log) and to the one that ran it, so the first non-empty log
/// wins, and when every log is empty the first broker.
pub fn choose_log(found: Vec<(String, String)>) -> Option<(String, String)> {
    let first = found.first().cloned();
    found
        .into_iter()
        .find(|(_, text)| !text.is_empty())
        .or(first)
}

/// `log <job>`.
pub async fn log(layout: &Layout, a: LogArgs) -> Result<i32, String> {
    let client = hub(layout).await?;
    let q = LogQuery {
        job: JobId(a.job),
        step: a.step,
        tail: a.tail,
        follow: a.follow,
    };
    let mut stdout = std::io::stdout();
    let json = a.json;
    let job = q.job.clone();
    let mut print = |text: &str| {
        if text.is_empty() {
            return;
        }
        let line = if json {
            format!("{}\n", json!({ "job": job, "text": text, "end": false }))
        } else {
            text.to_string()
        };
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.flush();
    };
    let redactor = cli_redactor(layout);
    let found = find_log(&client, a.broker.as_deref(), &q, &redactor, &mut print).await;
    client.close().await;
    let broker = found?;
    if json {
        println!("{}", json!({ "job": q.job, "broker": broker, "end": true }));
    }
    Ok(0)
}
