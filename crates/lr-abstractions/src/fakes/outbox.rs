use crate::store::{OutboxEntry, OutboxKind, StoreError};
use lr_core::{JobId, Timestamp};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct MemoryOutbox {
    counters: HashMap<(String, JobId), u64>,
    entries: Vec<(String, OutboxEntry)>,
}

impl MemoryOutbox {
    pub(crate) fn push(
        &mut self,
        recipient: &str,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
    ) -> u64 {
        let counter = self
            .counters
            .entry((recipient.to_string(), job.clone()))
            .or_insert(0);
        *counter += 1;
        let seq = *counter;
        self.entries.push((
            recipient.to_string(),
            OutboxEntry {
                job: job.clone(),
                seq,
                kind,
                payload,
                created_at: at,
            },
        ));
        seq
    }

    pub(crate) fn bytes_for(&self, recipient: &str) -> u64 {
        self.entries
            .iter()
            .filter(|(r, _)| r == recipient)
            .map(|(_, e)| e.payload.len() as u64)
            .sum()
    }

    pub(crate) fn pending(&self, recipient: Option<&str>) -> Vec<OutboxEntry> {
        let mut out: Vec<OutboxEntry> = self
            .entries
            .iter()
            .filter(|(r, _)| recipient.is_none_or(|want| r == want))
            .map(|(_, e)| e.clone())
            .collect();
        let first: HashMap<JobId, Timestamp> = out.iter().fold(HashMap::new(), |mut m, e| {
            let at = m.entry(e.job.clone()).or_insert(e.created_at);
            *at = (*at).min(e.created_at);
            m
        });
        out.sort_by(|a, b| (first[&a.job], &a.job, a.seq).cmp(&(first[&b.job], &b.job, b.seq)));
        out
    }

    pub(crate) fn ack(&mut self, recipient: &str, job: &JobId, seq: u64) -> usize {
        self.remove(|r, e| r == recipient && &e.job == job && e.seq <= seq)
    }

    pub(crate) fn discard(&mut self, recipient: &str, job: &JobId) -> usize {
        self.remove(|r, e| r == recipient && &e.job == job)
    }

    pub(crate) fn remove(&mut self, doomed: impl Fn(&str, &OutboxEntry) -> bool) -> usize {
        let before = self.entries.len();
        self.entries.retain(|(r, e)| !doomed(r, e));
        before - self.entries.len()
    }
}

pub(crate) fn accept_next(last: &mut u64, seq: u64) -> Result<bool, StoreError> {
    match seq {
        s if s <= *last => Ok(false),
        s if s == *last + 1 => {
            *last = s;
            Ok(true)
        }
        s => Err(StoreError::SeqGap {
            expected: *last + 1,
            got: s,
        }),
    }
}
