#![allow(dead_code)]

pub mod helper;
pub mod no_console;

use async_trait::async_trait;
use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{Clock, JobEvent, JobOutput, JobSpec};
use lr_core::{JobId, StepSpec, Timestamp};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[allow(unused_imports)]
pub use helper::{helper_command, Helper};

pub const T0: Timestamp = Timestamp(1_700_000_000_000);

#[test]
fn helper_process() {
    helper::run_if_requested();
}

pub fn clock() -> (ManualClock, Arc<dyn Clock>) {
    let clock = ManualClock::new(T0);
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    (clock, shared)
}

pub fn workdir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("lr-exec")
        .join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn fresh_file(dir: &str, name: &str) -> PathBuf {
    let path = workdir(dir).join(name);
    let _ = std::fs::remove_file(&path);
    path
}

pub fn step(name: &str, command: &str, timeout_s: u64) -> StepSpec {
    StepSpec {
        name: name.to_string(),
        command: command.to_string(),
        timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

pub fn base_env() -> Vec<(String, String)> {
    let keep = ["PATH", "TEMP", "TMP", "HOME"];
    std::env::vars()
        .filter(|(k, _)| keep.contains(&k.as_str()))
        .collect()
}

pub fn job(id: &str, timeout_s: u64, steps: Vec<StepSpec>) -> JobSpec {
    let mut spec = JobSpec::new(JobId(id.to_string()), timeout_s, steps);
    spec.cwd = workdir("job");
    spec.env = base_env();
    spec
}

pub fn events_text(events: &[JobEvent]) -> String {
    String::from_utf8_lossy(&events_bytes(events)).into_owned()
}

pub fn events_bytes(events: &[JobEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Output { data, .. } => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[derive(Default)]
pub struct Recorder {
    events: Mutex<Vec<JobEvent>>,
    notify: Notify,
    delay_first: AtomicUsize,
    block_after: Mutex<Option<usize>>,
    max_chunk: AtomicUsize,
}

impl Recorder {
    pub fn new() -> Arc<Recorder> {
        Arc::new(Recorder::default())
    }

    pub fn slow_first(self: &Arc<Self>, count: usize) -> Arc<Self> {
        self.delay_first.store(count, Ordering::SeqCst);
        self.clone()
    }

    pub fn block_after(self: &Arc<Self>, count: usize) -> Arc<Self> {
        *self.block_after.lock().unwrap() = Some(count);
        self.clone()
    }

    pub fn events(&self) -> Vec<JobEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn text(&self) -> String {
        events_text(&self.events())
    }

    pub fn max_chunk(&self) -> usize {
        self.max_chunk.load(Ordering::SeqCst)
    }

    pub async fn wait_for(&self, needle: &str) -> String {
        let found = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let notified = self.notify.notified();
                let text = self.text();
                if text.contains(needle) {
                    return text;
                }
                notified.await;
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("`{needle}` not seen in output: {}", self.text()))
    }

    pub async fn pid_of(&self, role: &str) -> u32 {
        let marker = format!("{role}-pid=");
        let text = self.wait_for(&format!("{role}-pid-end")).await;
        let rest = text.split(&marker).nth(1).expect("pid line");
        rest.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap()
    }
}

#[async_trait]
impl JobOutput for Recorder {
    async fn emit(&self, event: JobEvent) {
        let seen = self.events.lock().unwrap().len();
        let block_after = *self.block_after.lock().unwrap();
        if let Some(limit) = block_after {
            if seen >= limit {
                std::future::pending::<()>().await;
            }
        }
        if seen < self.delay_first.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if let JobEvent::Output { data, .. } = &event {
            self.max_chunk.fetch_max(data.len(), Ordering::SeqCst);
        }
        self.events.lock().unwrap().push(event);
        self.notify.notify_waiters();
    }
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

#[cfg(target_os = "linux")]
pub fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let state = stat.rsplit(')').next().unwrap_or("").trim_start();
            !state.starts_with('Z') && !state.starts_with('X')
        }
        Err(_) => false,
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

pub async fn real_sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

pub async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}
