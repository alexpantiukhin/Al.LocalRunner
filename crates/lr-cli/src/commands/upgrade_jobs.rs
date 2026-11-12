//! The jobs `upgrade` meets: what runs and waits now, and what the user decides about it —
//! wait for the running jobs, cancel them, or give up.

use super::confirm::{proceed, Console};
use super::view::{known_brokers, read};
use crate::paths::Layout;
use lr_proto::{ReadReply, ReadRequest, StatusFilter};

/// How the runners are stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Running jobs finish first.
    Wait,
    /// Running jobs are cancelled; the runners exit within 30 s.
    Cancel,
}

/// The jobs of the brokers the hub of this OS user knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// A line per running job.
    pub running: Vec<String>,
    /// Queued orders (the first part of each queue).
    pub queued: usize,
    /// Why the jobs are not known, when they are not.
    pub note: Option<String>,
}

impl Activity {
    /// Whether a job runs now.
    pub fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The lines shown before the question.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(note) = &self.note {
            out.push(note.clone());
        }
        if self.busy() {
            out.push(format!("running jobs: {}", self.running.len()));
            out.extend(self.running.iter().map(|l| format!("  {l}")));
        }
        if self.queued > 0 {
            out.push(format!(
                "waiting orders: {} (they stay queued; a stopping runner takes none)",
                self.queued
            ));
        }
        if out.is_empty() {
            out.push("no job runs or waits".into());
        }
        out
    }
}

/// Asks the hub of this OS user which jobs run and wait; without a hub the jobs are not known.
pub async fn activity(layout: &Layout) -> Activity {
    let Ok(client) = super::jobs::hub(layout).await else {
        return Activity {
            note: Some("the hub is not running: running jobs are not known".into()),
            ..Activity::default()
        };
    };
    let mut out = Activity::default();
    match known_brokers(&client).await {
        Err(e) => out.note = Some(format!("jobs are not known: {e}")),
        Ok(brokers) => {
            for b in brokers {
                if !b.available {
                    out.note = Some(format!(
                        "broker {} is unreachable: its jobs are not known",
                        b.name
                    ));
                    continue;
                }
                match read(
                    &client,
                    &b.name,
                    ReadRequest::Status(StatusFilter::default()),
                )
                .await
                {
                    Ok(ReadReply::Status(v)) => {
                        out.queued += v.queue.len();
                        out.running.extend(v.running.iter().map(|r| {
                            let s = &r.summary;
                            format!(
                                "{} on {} ({})",
                                s.job.0,
                                s.runner.as_deref().unwrap_or("?"),
                                s.profile.as_deref().unwrap_or(&s.client_label)
                            )
                        }));
                    }
                    Ok(other) => out.note = Some(format!("broker {}: {other:?}", b.name)),
                    Err(e) => out.note = Some(e),
                }
            }
        }
    }
    client.close().await;
    out
}

/// What the user decides about the jobs: the mode, or `None` when they give up (`aborted` is
/// printed). Without running jobs it is the plain question `question`. With them, `yes` waits
/// (or cancels with `cancel`); at a terminal the user picks; without a terminal nothing
/// goes on.
pub fn choose(
    activity: &Activity,
    yes: bool,
    cancel: bool,
    question: &str,
    console: &mut Console<'_>,
) -> Result<Option<Mode>, String> {
    let mode = if cancel { Mode::Cancel } else { Mode::Wait };
    let action = "upgrade stops the roles and replaces the binary";
    if !activity.busy() {
        return Ok(proceed(action, question, yes, console)?.then_some(Mode::Wait));
    }
    let n = activity.running.len();
    if yes {
        return Ok(Some(mode));
    }
    if !console.terminal {
        return Err(format!(
            "{n} job(s) run: upgrade waits for them or cancels them; confirm with --yes (wait) or --yes --cancel (cancel the running jobs)"
        ));
    }
    if cancel {
        let q = format!("cancel {n} running job(s) and upgrade?");
        return Ok(
            proceed("upgrade cancels the running jobs", &q, false, console)?
                .then_some(Mode::Cancel),
        );
    }
    let _ = write!(
        console.out,
        "{n} job(s) run. Wait for them to finish (it can take long; Ctrl+C stops waiting and nothing is replaced), cancel them now, or abort? [w]ait / [c]ancel / [a]bort: "
    );
    let _ = console.out.flush();
    let mut line = String::new();
    console
        .input
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(match line.trim().to_ascii_lowercase().as_str() {
        "w" | "wait" => Some(Mode::Wait),
        "c" | "cancel" => Some(Mode::Cancel),
        _ => {
            let _ = writeln!(console.out, "aborted");
            None
        }
    })
}
