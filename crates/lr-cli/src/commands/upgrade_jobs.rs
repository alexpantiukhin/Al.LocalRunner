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
    /// (listed only when there are any), and the queued orders stay. A runner the broker lists
    /// by its bare name (it joined over the network) is taken for this installation's by that
    /// name alone, so a runner of another machine with the same name is counted as well: a
    /// note names those jobs.
    pub fn cancel_lines(&self, local: &BTreeSet<String>) -> Vec<String> {
        let list = |ids: Vec<String>| match ids.is_empty() {
            true => "none".to_string(),
            false => ids.join(", "),
        };
        let (cancelled, elsewhere) = self.split(local);
        let by_name: Vec<String> = cancelled
            .iter()
            .filter(|j| j.runner.as_deref().is_some_and(|r| local.contains(r)))
            .map(|j| j.job.clone())
            .collect();
        let ids = |jobs: Vec<&RunningJob>| jobs.iter().map(|j| j.job.clone()).collect::<Vec<_>>();
        let mut out = vec![format!("cancelling: {}", list(ids(cancelled)))];
        if !by_name.is_empty() {
            out.push(format!(
                "note: {} run on a runner matched to this installation by name only; a same-named runner of another machine is not told apart and its job keeps running",
                by_name.join(", ")
            ));
        }
        if !elsewhere.is_empty() {
            out.push(format!("still running elsewhere: {}", list(ids(elsewhere))));
        }
        out.push(format!("staying queued: {}", list(self.queued.clone())));
        out
    }

    /// The running jobs split into those on the runners of this installation (`local`, by name)
    /// and the others.
    pub fn split(&self, local: &BTreeSet<String>) -> (Vec<&RunningJob>, Vec<&RunningJob>) {
        self.running_jobs.iter().partition(|j| {
            j.runner
                .as_deref()
                .is_some_and(|r| local.iter().any(|name| is_own_name(r, name)))
        })
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
                running_ms: s.started_at.map(|t| now().0.saturating_sub(t.0)),
            });
        }
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
                            Ok((*v, next))
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
    /// How long it has run, in ms, when known.
    pub running_ms: Option<u64>,
}

const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// What `upgrade` prints while it waits for the running jobs: the lines at the start, then one
/// line when the set of running jobs changes and one line every minute, so a long wait stays
/// short in a terminal and in a log.
#[derive(Debug, Clone)]
pub struct WaitProgress {
    jobs: BTreeSet<String>,
    printed: std::time::Duration,
}

impl WaitProgress {
    /// The progress of a wait that starts with `activity`, and the lines printed at its start:
    /// what is waited for, that no new job is taken meanwhile, and a line per running job.
    pub fn start(activity: &Activity) -> (Self, Vec<String>) {
        let n = activity.running.len();
        let mut lines = vec![format!(
            "waiting for {} to finish; new jobs are not taken while upgrade waits (Ctrl+C stops waiting, nothing is replaced)",
            jobs_word(n)
        )];
        lines.extend(activity.running.iter().map(|l| format!("  {l}")));
        let jobs = activity
            .running_jobs
            .iter()
            .map(|j| j.job.clone())
            .collect();
        let progress = Self {
            jobs,
            printed: std::time::Duration::ZERO,
        };
        (progress, lines)
    }

    /// The line to print for the jobs running `elapsed` after the wait began: when the set of
    /// jobs changed or a minute passed since the last line; otherwise nothing.
    pub fn observe(&mut self, jobs: &[RunningJob], elapsed: std::time::Duration) -> Option<String> {
        let ids: BTreeSet<String> = jobs.iter().map(|j| j.job.clone()).collect();
        if ids == self.jobs && elapsed < self.printed + PROGRESS_EVERY {
            return None;
        }
        self.jobs = ids;
        self.printed = elapsed;
        if jobs.is_empty() {
            return Some("the jobs have finished, waiting for the runners to stop".into());
        }
        let longest = jobs.iter().filter_map(|j| j.running_ms).max();
        Some(match longest {
            Some(ms) => format!("waiting for {}, running {}", jobs_word(jobs.len()), age(ms)),
            None => format!("waiting for {}", jobs_word(jobs.len())),
        })
    }
}

fn jobs_word(n: usize) -> String {
    match n {
        1 => "1 job".into(),
        n => format!("{n} jobs"),
    }
}

fn age(ms: u64) -> String {
    let minutes = ms / 60_000;
    match ms {
        0..=59_999 => format!("{} s", ms / 1000),
        _ if minutes < 60 => format!("{minutes} min"),
        _ => format!("{} h {:02} min", minutes / 60, minutes % 60),
    }
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
/// goes on. The cancel question counts only the jobs on the runners of this installation
/// (`local`), the ones a cancel stops.
pub fn choose(
    activity: &Activity,
    local: &BTreeSet<String>,
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
        let (here, elsewhere) = activity.split(local);
        let others = match elsewhere.len() {
            0 => String::new(),
            k => format!(" ({k} on other runners keep running)"),
        };
        let q = format!("cancel {} running job(s){others} and upgrade?", here.len());
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
