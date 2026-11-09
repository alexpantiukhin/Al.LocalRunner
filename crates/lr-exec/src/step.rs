use crate::spawn::Spawned;
use lr_abstractions::{Clock, JobEvent, JobOutput, Sleep, Stop, StopSignal};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{sleep_until, timeout, Instant};

const POLL: Duration = Duration::from_millis(20);
const OBSERVE_EVERY: u32 = 5;
const FLUSH_LIMIT: Duration = Duration::from_secs(1);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

type Emit<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Exited(i32),
    Timeout,
    Cancelled,
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

pub(crate) struct Limits<'c> {
    pub(crate) deadline: Sleep,
    pub(crate) grace: Duration,
    pub(crate) clock: &'c dyn Clock,
}

struct Run {
    decided: Option<Ended>,
    exited: bool,
    killed: bool,
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
) -> Ended {
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
    let mut pending: Option<Vec<u8>> = None;
    let mut inflight: Option<Emit<'a>> = None;
    let mut tick = tokio::time::interval(POLL);
    let mut ticks: u32 = 0;
    loop {
        if run.exited && run.tree_empty && !run.open && pending.is_none() && inflight.is_none() {
            break;
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
                run.killed = true;
            }
            got = stop.reached(run.watching.unwrap_or(Stop::Kill)), if run.watching.is_some() => {
                run.watching = if got == Stop::Cancel { Some(Stop::Kill) } else { None };
                run.bound_output();
                let first = run.decided.is_none();
                run.decide(Ended::Cancelled);
                if first && got == Stop::Cancel {
                    run.grace = Some(limits.clock.sleep(limits.grace));
                    tree.soft_stop();
                } else if !run.killed {
                    tree.kill();
                    run.killed = true;
                }
            }
            () = async { run.grace.as_mut().unwrap().await }, if run.grace.is_some() && !run.killed => {
                tree.kill();
                run.killed = true;
            }
            status = child.wait(), if !run.exited => {
                run.exited = true;
                run.decide(Ended::Exited(exit_code(status)));
                if run.grace.is_none() {
                    tree.kill();
                    run.killed = true;
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
                if ticks.is_multiple_of(OBSERVE_EVERY) && !run.tree_empty {
                    tree.observe();
                }
                if run.killed && !run.tree_empty {
                    tree.kill();
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
    run.decided.unwrap_or(Ended::Cancelled)
}
