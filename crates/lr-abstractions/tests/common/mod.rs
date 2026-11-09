#![allow(dead_code)]

use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{Clock, JobSpec, Peer};
use lr_core::{Endpoint, JobId, StepSpec, Timestamp};
use std::sync::Arc;

pub const T0: Timestamp = Timestamp(1_700_000_000_000);

pub fn clock() -> (ManualClock, Arc<dyn Clock>) {
    let clock = ManualClock::new(T0);
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    (clock, shared)
}

pub fn ipc_peer(user: &str) -> Peer {
    Peer {
        endpoint: Endpoint::Ipc {
            os_user: user.to_string(),
        },
        key: None,
    }
}

pub fn net_peer(addr: &str, key: &str) -> Peer {
    Peer {
        endpoint: Endpoint::Addr(addr.parse().unwrap()),
        key: Some(key.to_string()),
    }
}

pub fn step(name: &str, timeout_s: u64) -> StepSpec {
    StepSpec {
        name: name.to_string(),
        command: format!("run {name}"),
        timeout_s,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

pub fn job(id: &str, timeout_s: u64, steps: Vec<StepSpec>) -> JobSpec {
    JobSpec::new(JobId(id.to_string()), timeout_s, steps)
}

pub async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}
