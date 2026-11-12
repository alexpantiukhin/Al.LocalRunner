use crate::limits::oom_exceeded;
use crate::spawn::Spawned;
use crate::watch::Watch;
use lr_abstractions::{
    Clock, Enforce, Exceeded, JobEvent, JobOutput, ResourceLimits, Sleep, Stop, StopSignal, Usage,
};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{sleep_until, timeout, Instant};

const POLL: Duration = Duration::from_millis(20);
const OBSERVE_EVERY: u32 = 5;
const SAMPLE_EVERY: u32 = 12;
const FLUSH_LIMIT: Duration = Duration::from_secs(1);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);
const KILL_LIMIT: Duration = Duration::from_secs(10);

type Emit<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited(i32),
    Timeout,
    Cancelled,
    Exceeded,
}

pub(crate) struct Emitter<'a> {
    pub(crate) output: &'a dyn JobOutput,
    pub(crate) bounded: bool,
    pub(crate) line_open: bool,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(output: &'a dyn JobOutput) -> Emitter<'a> {
        Emitter {
            output,
            bounded: false,
            line_open: false,
        }
    }

    fn emit(&mut self, event: JobEvent) -> Emit<'a> {
        if let JobEvent::Output { data, .. } = &event {
            if let Some(&last) = data.last() {
                self.line_open = last != b'\n';
            }
        }
        self.output.emit(event)
    }

    pub(crate) async fn send(&mut self, event: JobEvent, stop: &mut StopSignal) {
        let mut emit = self.emit(event);
        if !self.bounded {
            tokio::select! {
                biased;
                () = &mut emit => return,
                _ = stop.reached(Stop::Cancel) => self.bounded = true,
            }
        }
        let _ = timeout(FLUSH_LIMIT, emit).await;
    }
}

pub(crate) struct Finished {
    pub(crate) ended: Ended,
    pub(crate) soft_failed: bool,
    pub(crate) usage: Usage,
    pub(crate) exceeded: Option<Exceeded>,
}

pub(crate) struct Limits<'c> {
    pub(crate) deadline: Sleep,
    pub(crate) grace: Duration,
    pub(crate) clock: &'c dyn Clock,
    pub(crate) resources: &'c ResourceLimits,
}

struct Run {
    decided: Option<Ended>,
    exited: bool,
    killed: bool,
    killed_at: Option<Instant>,
    soft_failed: bool,
    deadline_done: bool,
    watching: Option<Stop>,
    grace: Option<Sleep>,
    flush_until: Option<Instant>,
    drain_until: Option<Instant>,
    tree_empty: bool,
    open: bool,
}

impl Run {
    fn decide(&mut self, ended: Ended) {
        if self.decided.is_none() {
            self.decided = Some(ended);
        }
    }

    fn mark_killed(&mut self) {
        self.killed = true;
        self.killed_at.get_or_insert_with(Instant::now);
    }

    fn kill_overdue(&self) -> bool {
        self.killed_at
            .is_some_and(|at| Instant::now().duration_since(at) >= KILL_LIMIT)
    }

    fn bound_output(&mut self) {
        if self.flush_until.is_none() {
            self.flush_until = Some(Instant::now() + FLUSH_LIMIT);
        }
    }
}

fn exit_code(status: std::io::Result<std::process::ExitStatus>) -> i32 {
    match status {
        Ok(status) => status.code().unwrap_or_else(|| signal_code(&status)),
        Err(_) => -1,
    }
}

#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map_or(-1, |s| 128 + s)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> i32 {
    -1
}

pub(crate) async fn supervise<'a>(
    spawned: Spawned,
    mut limits: Limits<'_>,
    index: usize,
    stop: &mut StopSignal,
    emitter: &mut Emitter<'a>,
) -> Finished {
    let Spawned {
        mut child,
        tree,
        mut output,
        mut readers,
    } = spawned;
    let mut run = Run {
        decided: None,
        exited: false,
        killed: false,
        killed_at: None,
        soft_failed: false,
        deadline_done: false,
        watching: Some(Stop::Cancel),
        grace: None,
        flush_until: None,
        drain_until: None,
        tree_empty: false,
        open: true,
    };
    if emitter.bounded {
        run.bound_output();
    }
    let mut watch = Watch::new(limits.resources);
    let track_oom = limits.resources.mode == Enforce::Kill && limits.resources.mem_mb.is_some();
    let mut exceeded: Option<Exceeded> = None;
    let mut notes: Vec<u8> = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    let mut inflight: Option<Emit<'a>> = None;
    let mut tick = tokio::time::interval(POLL);
    let mut ticks: u32 = 0;
    loop {
        if run.exited && run.tree_empty && !run.open && pending.is_none() && inflight.is_none() {
            break;
        }
        if pending.is_none() && !notes.is_empty() {
            pending = Some(std::mem::take(&mut notes));
        }
        if inflight.is_none() {
            if let Some(data) = pending.take() {
                if !emitter.bounded {
                    inflight = Some(emitter.emit(JobEvent::Output { index, data }));
                }
            }
        }
        let flush_at = run.flush_until.filter(|_| !emitter.bounded);
        tokio::select! {
            biased;
            () = &mut limits.deadline, if !run.deadline_done => {
                run.deadline_done = true;
                run.decide(Ended::Timeout);
                run.bound_output();
                tree.kill();
                run.mark_killed();
            }
            got = stop.reached(run.watching.unwrap_or(Stop::Kill)), if run.watching.is_some() => {
                run.watching = if got == Stop::Cancel { Some(Stop::Kill) } else { None };
                run.bound_output();
                let first = run.decided.is_none();
                run.decide(Ended::Cancelled);
                if first && got == Stop::Cancel {
                    run.grace = Some(limits.clock.sleep(limits.grace));
                    run.soft_failed = !tree.soft_stop();
                } else if !run.killed {
                    tree.kill();
                    run.mark_killed();
                }
            }
            () = async { run.grace.as_mut().unwrap().await }, if run.grace.is_some() && !run.killed => {
                tree.kill();
                run.mark_killed();
            }
            status = child.wait(), if !run.exited => {
                run.exited = true;
                let code = exit_code(status);
                let sampled = watch.sample(&tree);
                let open_line = line_open(&pending, emitter.line_open);
                add_notes(&mut notes, sampled.warnings, open_line);
                let oom = if track_oom && run.decided.is_none() {
                    tree.refresh_memory_evidence();
                    oom_exceeded(
                        limits.resources,
                        code,
                        tree.memory_evidence(),
                        watch.peak_mem_mb(),
                    )
                } else {
                    None
                };
                match sampled.kill.or(oom) {
                    Some(ex) if code != 0 && run.decided.is_none() => {
                        watch.raise_mem_peak(ex.peak_mb);
                        exceeded = Some(ex);
                        run.decide(Ended::Exceeded);
                    }
                    _ => run.decide(Ended::Exited(code)),
                }
                if run.grace.is_none() {
                    tree.kill();
                    run.mark_killed();
                }
            }
            () = async { inflight.as_mut().unwrap().await }, if inflight.is_some() => {
                inflight = None;
            }
            () = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                emitter.bounded = true;
                inflight = None;
                pending = None;
            }
            got = output.recv(), if run.open && pending.is_none() => match got {
                Some(data) => pending = Some(data),
                None => run.open = false,
            },
            _ = tick.tick() => {
                ticks = ticks.wrapping_add(1);
                if ticks % SAMPLE_EVERY == 1 && !run.tree_empty && !run.killed {
                    let sampled = watch.sample(&tree);
                    let open_line = line_open(&pending, emitter.line_open) && notes.is_empty();
                    add_notes(&mut notes, sampled.warnings, open_line);
                    if let Some(ex) = sampled.kill {
                        if run.decided.is_none() {
                            exceeded = Some(ex);
                            run.decide(Ended::Exceeded);
                            run.bound_output();
                            tree.kill();
                            run.mark_killed();
                        }
                    }
                }
                if ticks.is_multiple_of(OBSERVE_EVERY) && !run.tree_empty {
                    tree.observe();
                }
                if run.killed && !run.tree_empty {
                    tree.kill();
                    if run.kill_overdue() {
                        run.exited = true;
                        run.decide(Ended::Cancelled);
                        run.tree_empty = true;
                        run.drain_until = Some(Instant::now());
                    }
                }
                if run.exited && !run.tree_empty {
                    run.tree_empty = tree.is_empty();
                    if run.tree_empty {
                        run.drain_until = Some(Instant::now() + DRAIN_LIMIT);
                    }
                }
                if run.drain_until.is_some_and(|t| Instant::now() >= t) && run.open {
                    run.open = false;
                    pending = None;
                    inflight = None;
                    for reader in &readers {
                        reader.abort();
                    }
                }
            }
        }
    }
    for reader in readers.drain(..) {
        reader.abort();
    }
    Finished {
        ended: run.decided.unwrap_or(Ended::Cancelled),
        soft_failed: run.soft_failed,
        usage: watch.usage(),
        exceeded,
    }
}

fn line_open(pending: &Option<Vec<u8>>, emitted_open: bool) -> bool {
    pending
        .as_ref()
        .and_then(|p| p.last())
        .map_or(emitted_open, |&b| b != b'\n')
}

fn add_notes(notes: &mut Vec<u8>, warnings: Vec<u8>, line_open: bool) {
    if warnings.is_empty() {
        return;
    }
    if line_open {
        notes.push(b'\n');
    }
    notes.extend_from_slice(&warnings);
}
