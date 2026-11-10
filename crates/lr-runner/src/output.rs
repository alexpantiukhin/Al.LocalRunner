use crate::outbox::Outbox;
use async_trait::async_trait;
use lr_abstractions::{JobEvent, JobOutcome, JobOutput, StepOutcome};
use lr_core::JobId;
use lr_proto::{split_text, Chunk, RunnerToBroker, StepResult, CHUNK_TEXT_LIMIT};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Utf8Stream {
    held: Vec<u8>,
}

impl Utf8Stream {
    pub(crate) fn feed(&mut self, data: &[u8]) -> String {
        self.held.extend_from_slice(data);
        let mut out = String::new();
        let mut rest: &[u8] = &self.held;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(good).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            rest = &bad[n..];
                        }
                        None => {
                            rest = bad;
                            break;
                        }
                    }
                }
            }
        }
        self.held = rest.to_vec();
        out
    }

    pub(crate) fn finish(&mut self) -> String {
        let tail = String::from_utf8_lossy(&self.held).into_owned();
        self.held.clear();
        tail
    }
}

pub(crate) struct Forward<'a> {
    pub(crate) outbox: &'a Outbox,
    pub(crate) job: JobId,
    pub(crate) ords: Vec<Option<u32>>,
    pub(crate) streams: Mutex<HashMap<usize, Utf8Stream>>,
    pub(crate) reported: Mutex<HashSet<usize>>,
}

impl<'a> Forward<'a> {
    pub(crate) fn new(outbox: &'a Outbox, job: JobId, ords: Vec<Option<u32>>) -> Forward<'a> {
        Forward {
            outbox,
            job,
            ords,
            streams: Mutex::new(HashMap::new()),
            reported: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) async fn report_rest(&self, outcome: &JobOutcome) {
        for (index, step) in outcome.steps.iter().enumerate() {
            let done = self.reported.lock().map_or(true, |r| r.contains(&index));
            if !done {
                self.finished(index, step.clone()).await;
            }
        }
    }

    fn decode(&self, index: usize, data: Option<&[u8]>) -> String {
        let Ok(mut streams) = self.streams.lock() else {
            return String::new();
        };
        let s = streams.entry(index).or_default();
        match data {
            Some(d) => s.feed(d),
            None => s.finish(),
        }
    }

    async fn chunks(&self, index: usize, text: &str) {
        let step = self.ords.get(index).copied().flatten();
        for piece in split_text(text, CHUNK_TEXT_LIMIT) {
            let msg = RunnerToBroker::Chunk(Chunk {
                job: self.job.clone(),
                seq: 0,
                step,
                text: piece.to_string(),
            });
            self.outbox.push_or_log(&self.job, &msg).await;
        }
    }

    async fn finished(&self, index: usize, outcome: StepOutcome) {
        if let Ok(mut r) = self.reported.lock() {
            r.insert(index);
        }
        let tail = self.decode(index, None);
        self.chunks(index, &tail).await;
        let Some(ord) = self.ords.get(index).copied().flatten() else {
            return;
        };
        let duration_ms = match (outcome.started_at, outcome.finished_at) {
            (Some(a), Some(b)) => b.millis_since(a),
            _ => 0,
        };
        let msg = RunnerToBroker::StepResult(StepResult {
            job: self.job.clone(),
            seq: 0,
            ord,
            name: outcome.name,
            state: outcome.state,
            exit_code: outcome.exit_code,
            duration_ms,
        });
        self.outbox.push_or_log(&self.job, &msg).await;
    }
}

#[async_trait]
impl JobOutput for Forward<'_> {
    async fn emit(&self, event: JobEvent) {
        match event {
            JobEvent::StepStarted { .. } => {}
            JobEvent::Output { index, data } => {
                let text = self.decode(index, Some(&data));
                self.chunks(index, &text).await;
            }
            JobEvent::StepFinished { index, outcome } => self.finished(index, outcome).await,
        }
    }
}
