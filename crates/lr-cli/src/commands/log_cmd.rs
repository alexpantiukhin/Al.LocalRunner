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
use lr_obs::Redactor;
use lr_proto::{CliRead, CliReply, CliRequest, ReadReply, ReadRequest};
use serde_json::json;
use std::io::Write;

const MAX_PENDING: usize = 64 * 1024;

/// Turns log text into redacted output: only whole lines pass, so a secret split across two
/// chunks is still masked; a line longer than 64 KiB passes in parts.
pub struct LogPrinter {
    redactor: Redactor,
    pending: String,
}

impl Default for LogPrinter {
    fn default() -> LogPrinter {
        LogPrinter {
            redactor: Redactor::new(&[]),
            pending: String::new(),
        }
    }
}

impl LogPrinter {
    /// The redacted whole lines that `text` completes.
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let cut = match self.pending.rfind('\n') {
            Some(i) => i + 1,
            None if self.pending.len() > MAX_PENDING => self.pending.len(),
            None => return String::new(),
        };
        let done: String = self.pending.drain(..cut).collect();
        self.redactor.redact_text(&done)
    }

    /// The redacted rest after the last line end.
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        self.redactor.redact_text(&rest)
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
    out: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let (names, mut problems) = reachable(client, broker).await?;
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
        let mut printer = LogPrinter::default();
        loop {
            match client.recv::<CliReply>().await? {
                CliReply::LogChunk { text, end, .. } => {
                    out(&printer.push(&text));
                    if end {
                        out(&printer.finish());
                        return Ok(b);
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
    problems.insert(
        0,
        format!("job {} is not found on the brokers of this hub", q.job),
    );
    Err(problems.join("\n"))
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
    let found = find_log(&client, a.broker.as_deref(), &q, &mut print).await;
    client.close().await;
    let broker = found?;
    if json {
        println!("{}", json!({ "job": q.job, "broker": broker, "end": true }));
    }
    Ok(0)
}
