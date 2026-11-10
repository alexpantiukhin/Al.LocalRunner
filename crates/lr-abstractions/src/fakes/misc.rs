use crate::datasource::{CancelRequest, DataError, DataSource, EventStream};
use crate::logsink::{LogSink, Record};
use crate::meter::Meter;
use crate::secrets::{SecretError, SecretSource, SecretValue};
use async_trait::async_trait;
use lr_core::JobId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// A secret store in memory; can be locked to make every read `Unavailable`.
#[derive(Default)]
pub struct MemorySecrets {
    values: Mutex<HashMap<String, SecretValue>>,
    locked: Mutex<Option<String>>,
}

impl MemorySecrets {
    /// An empty store.
    pub fn new() -> MemorySecrets {
        MemorySecrets::default()
    }

    /// Sets a secret.
    pub fn insert(&self, name: &str, value: &str) {
        self.values
            .lock()
            .unwrap()
            .insert(name.to_string(), SecretValue::new(value));
    }

    /// Locks the store with `reason`.
    pub fn lock(&self, reason: &str) {
        *self.locked.lock().unwrap() = Some(reason.to_string());
    }
}

#[async_trait]
impl SecretSource for MemorySecrets {
    async fn get(&self, name: &str) -> Result<SecretValue, SecretError> {
        if let Some(reason) = self.locked.lock().unwrap().clone() {
            return Err(SecretError::Unavailable {
                name: name.to_string(),
                reason,
            });
        }
        self.values
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(name.to_string()))
    }
}

/// A log sink in memory; when bounded it drops and counts what does not fit, like a real
/// sink with a full buffer.
#[derive(Default)]
pub struct MemoryLogSink {
    records: Mutex<Vec<Record>>,
    capacity: Option<usize>,
    dropped: Mutex<u64>,
}

impl MemoryLogSink {
    /// An unbounded sink.
    pub fn new() -> MemoryLogSink {
        MemoryLogSink::default()
    }

    /// A sink keeping at most `capacity` records.
    pub fn bounded(capacity: usize) -> MemoryLogSink {
        MemoryLogSink {
            capacity: Some(capacity),
            ..MemoryLogSink::default()
        }
    }

    /// Records kept so far.
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }
}

impl LogSink for MemoryLogSink {
    fn write(&self, record: &Record) {
        let mut records = self.records.lock().unwrap();
        if self.capacity.is_some_and(|c| records.len() >= c) {
            *self.dropped.lock().unwrap() += 1;
        } else {
            records.push(record.clone());
        }
    }

    fn dropped(&self) -> u64 {
        *self.dropped.lock().unwrap()
    }
}

/// One update a [`MemoryMeter`] received.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterUpdate {
    /// A counter increment.
    Add(String, Vec<(String, String)>, u64),
    /// A gauge value.
    Set(String, Vec<(String, String)>, f64),
    /// A histogram observation.
    Observe(String, Vec<(String, String)>, f64),
    /// Series with this label value forgotten.
    Forget(String, String),
}

/// A meter in memory: keeps every update in arrival order, accepts any name.
#[derive(Default)]
pub struct MemoryMeter {
    updates: Mutex<Vec<MeterUpdate>>,
}

impl MemoryMeter {
    /// An empty meter.
    pub fn new() -> MemoryMeter {
        MemoryMeter::default()
    }

    /// Updates received so far.
    pub fn updates(&self) -> Vec<MeterUpdate> {
        self.updates.lock().unwrap().clone()
    }
}

fn owned(labels: &[(&str, &str)]) -> Vec<(String, String)> {
    labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Meter for MemoryMeter {
    fn add(&self, name: &str, labels: &[(&str, &str)], delta: u64) {
        let update = MeterUpdate::Add(name.to_string(), owned(labels), delta);
        self.updates.lock().unwrap().push(update);
    }

    fn set(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        let update = MeterUpdate::Set(name.to_string(), owned(labels), value);
        self.updates.lock().unwrap().push(update);
    }

    fn observe(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        let update = MeterUpdate::Observe(name.to_string(), owned(labels), value);
        self.updates.lock().unwrap().push(update);
    }

    fn forget(&self, label: &str, value: &str) {
        let update = MeterUpdate::Forget(label.to_string(), value.to_string());
        self.updates.lock().unwrap().push(update);
    }
}

struct Feed<S, E> {
    snapshot: S,
    subscribers: Vec<mpsc::UnboundedSender<E>>,
    logs: HashMap<JobId, Vec<u8>>,
    cancels: Vec<(JobId, CancelRequest)>,
    unavailable: Option<String>,
}

/// A data source serving a settable snapshot, published events, logs and recorded cancels.
pub struct FakeDataSource<S, E> {
    feed: Arc<Mutex<Feed<S, E>>>,
}

impl<S: Clone + Send + 'static, E: Clone + Send + 'static> FakeDataSource<S, E> {
    /// A source serving `snapshot`.
    pub fn new(snapshot: S) -> FakeDataSource<S, E> {
        FakeDataSource {
            feed: Arc::new(Mutex::new(Feed {
                snapshot,
                subscribers: Vec::new(),
                logs: HashMap::new(),
                cancels: Vec::new(),
                unavailable: None,
            })),
        }
    }

    /// Replaces the snapshot.
    pub fn set_snapshot(&self, snapshot: S) {
        self.feed.lock().unwrap().snapshot = snapshot;
    }

    /// Sends `event` to every live subscriber.
    pub fn publish(&self, event: E) {
        self.feed
            .lock()
            .unwrap()
            .subscribers
            .retain(|s| s.send(event.clone()).is_ok());
    }

    /// Sets the saved log of a job.
    pub fn set_log(&self, job: JobId, log: Vec<u8>) {
        self.feed.lock().unwrap().logs.insert(job, log);
    }

    /// Makes every call fail with `Unavailable(reason)`, or serve again with `None`.
    pub fn set_unavailable(&self, reason: Option<String>) {
        self.feed.lock().unwrap().unavailable = reason;
    }

    /// Cancels requested so far.
    pub fn cancels(&self) -> Vec<(JobId, CancelRequest)> {
        self.feed.lock().unwrap().cancels.clone()
    }

    fn check(&self) -> Result<(), DataError> {
        match &self.feed.lock().unwrap().unavailable {
            Some(r) => Err(DataError::Unavailable(r.clone())),
            None => Ok(()),
        }
    }
}

struct ChannelStream<E>(mpsc::UnboundedReceiver<E>);

#[async_trait]
impl<E: Send> EventStream<E> for ChannelStream<E> {
    async fn next(&mut self) -> Option<E> {
        self.0.recv().await
    }
}

#[async_trait]
impl<S, E> DataSource for FakeDataSource<S, E>
where
    S: Clone + Send + 'static,
    E: Clone + Send + 'static,
{
    type Snapshot = S;
    type Event = E;

    async fn snapshot(&self) -> Result<S, DataError> {
        self.check()?;
        Ok(self.feed.lock().unwrap().snapshot.clone())
    }

    async fn subscribe(&self) -> Result<Box<dyn EventStream<E>>, DataError> {
        self.check()?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.feed.lock().unwrap().subscribers.push(tx);
        Ok(Box::new(ChannelStream(rx)))
    }

    async fn log(&self, job: &JobId) -> Result<Vec<u8>, DataError> {
        self.check()?;
        self.feed
            .lock()
            .unwrap()
            .logs
            .get(job)
            .cloned()
            .ok_or(DataError::NotFound)
    }

    async fn cancel(&self, job: &JobId, request: CancelRequest) -> Result<(), DataError> {
        self.check()?;
        self.feed
            .lock()
            .unwrap()
            .cancels
            .push((job.clone(), request));
        Ok(())
    }
}
