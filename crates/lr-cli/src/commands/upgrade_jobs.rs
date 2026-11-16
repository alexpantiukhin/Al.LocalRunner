//! The jobs `upgrade` meets: what runs and waits now, and what the user decides about it —
//! wait for the running jobs, cancel them, or give up.

use super::confirm::{proceed, Console};
use super::timefmt::{format_ms, now};
use super::upgrade_pool::is_own_name;
use super::view::{known_brokers, read};
use crate::paths::Layout;
use lr_proto::{ReadReply, ReadRequest, StatusFilter, StatusView, STATUS_LIMIT_MAX};
use std::collections::BTreeSet;
use std::future::Future;

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
    /// The running jobs and the runners they run on, in the order of `running`.
    pub running_jobs: Vec<RunningJob>,
    /// The ids of all the queued orders.
    pub queued: Vec<String>,
    /// Why the jobs are not known, when they are not.
    pub note: Option<String>,
}

impl Activity {
    /// Whether a job runs now.
    pub fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// What `upgrade --cancel` does to the jobs, each as a full list: the running jobs of the
    /// runners it stops (`local`, by name) are cancelled, the jobs of other runners keep running
    /// (listed only when there are any), and the queued orders stay.
    pub fn cancel_lines(&self, local: &BTreeSet<String>) -> Vec<String> {
        let list = |ids: Vec<String>| match ids.is_empty() {
            true => "none".to_string(),
            false => ids.join(", "),
        };
        let (cancelled, elsewhere): (Vec<&RunningJob>, Vec<&RunningJob>) =
            self.running_jobs.iter().partition(|j| {
                j.runner
                    .as_deref()
                    .is_some_and(|r| local.iter().any(|name| is_own_name(r, name)))
            });
        let ids = |jobs: Vec<&RunningJob>| jobs.iter().map(|j| j.job.clone()).collect::<Vec<_>>();
        let mut out = vec![format!("cancelling: {}", list(ids(cancelled)))];
        if !elsewhere.is_empty() {
            out.push(format!("still running elsewhere: {}", list(ids(elsewhere))));
        }
        out.push(format!("staying queued: {}", list(self.queued.clone())));
        out
    }

    /// Adds the jobs of one broker's whole `status` (every page of its queue).
    pub fn add(&mut self, v: &StatusView) {
        self.queued
            .extend(v.queue.iter().map(|q| q.summary.job.0.clone()));
        for r in &v.running {
            let s = &r.summary;
            let running = s.started_at.map_or(String::new(), |t| {
                format!(", running {}", format_ms(now().0.saturating_sub(t.0)))
            });
            self.running.push(format!(
                "{} on {} ({}){running}",
                s.job.0,
                s.runner.as_deref().unwrap_or("?"),
                s.profile.as_deref().unwrap_or(&s.client_label)
            ));
            self.running_jobs.push(RunningJob {
                job: s.job.0.clone(),
                runner: s.runner.clone(),
            });
        }
    }

    /// The lines shown while `upgrade` waits: a line per running job, then the time waited.
    pub fn waiting_lines(&self, elapsed: std::time::Duration) -> Vec<String> {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let mut out = self.running.clone();
        out.push(format!("waited {}", format_ms(ms)));
        out
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
        if !self.queued.is_empty() {
            out.push(format!(
                "waiting orders: {} (they stay queued; a stopping runner takes none): {}",
                self.queued.len(),
                self.queued.join(", ")
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
                let (client, name) = (&client, &b.name);
                let read_page = |cursor: Option<String>| async move {
                    let filter = StatusFilter {
                        limit: Some(STATUS_LIMIT_MAX),
                        cursor,
                        ..StatusFilter::default()
                    };
                    match read(client, name, ReadRequest::Status(filter)).await? {
                        ReadReply::Status(v) => {
                            let next = v.next.clone();
                            Ok((v, next))
                        }
                        other => Err(format!("broker {name}: {other:?}")),
                    }
                };
                match pages(read_page).await {
                    Ok(views) => {
                        let mut whole = views.into_iter();
                        if let Some(mut first) = whole.next() {
                            first.queue.extend(whole.flat_map(|v| v.queue));
                            out.add(&first);
                        }
                    }
                    Err(e) => out.note = Some(e),
                }
            }
        }
    }
    client.close().await;
    out
}

/// A running job and the runner it runs on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningJob {
    /// The job.
    pub job: String,
    /// The runner, as the broker names it.
    pub runner: Option<String>,
}

/// Every page of a paged answer: `ask` gets the token of the previous page (`None` first) and
/// returns a page and the token of the next one; reading stops at the end (no token) or when a
/// token comes back unchanged.
pub async fn pages<T, F, Fut>(mut ask: F) -> Result<Vec<T>, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(T, Option<String>), String>>,
{
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let (page, next) = ask(cursor.clone()).await?;
        out.push(page);
        match next {
            Some(n) if Some(&n) != cursor.as_ref() => cursor = Some(n),
            _ => return Ok(out),
        }
    }
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
