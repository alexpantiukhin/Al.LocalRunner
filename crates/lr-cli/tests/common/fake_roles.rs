use async_trait::async_trait;
use lr_cli::commands::down_cmd::{Roles, ServiceState};
use lr_cli::commands::upgrade_pool::PoolRunner;
use lr_cli::paths::Layout;
use lr_proto::{DownMode, DrainReason};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// An installation in `dir` with runner `r1`.
pub fn with_runner(dir: &super::TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(root.join("runner").join("r1")).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    Layout::new(root, config)
}

/// Running roles whose runners keep answering for `answers_left` probes (`usize::MAX`: they
/// never exit); the broker is an OS service when `service`.
pub struct FakeRoles {
    pub service: bool,
    answers_left: AtomicUsize,
    pub told: Mutex<Vec<(String, DownMode)>>,
    pub reasons: Mutex<Vec<Option<DrainReason>>>,
    pub lock_file: Mutex<Option<PathBuf>>,
    pub lock_seen: Mutex<Vec<bool>>,
    pub hub_stops: AtomicUsize,
    pub broker_stops: AtomicUsize,
    pub jobs: Mutex<Option<Vec<String>>>,
    pub said: Mutex<Vec<String>>,
    pub said_err: Mutex<Vec<String>>,
    pub broker_unreachable: bool,
    pub broker_state: ServiceState,
    pub after_restart: Mutex<Vec<PoolRunner>>,
    pub service_runner: Option<Option<bool>>,
    pub service_unreachable: bool,
}

impl FakeRoles {
    pub fn new(service: bool, answers_left: usize) -> FakeRoles {
        FakeRoles {
            service,
            answers_left: AtomicUsize::new(answers_left),
            told: Mutex::new(Vec::new()),
            reasons: Mutex::new(Vec::new()),
            lock_file: Mutex::new(None),
            lock_seen: Mutex::new(Vec::new()),
            hub_stops: AtomicUsize::new(0),
            broker_stops: AtomicUsize::new(0),
            jobs: Mutex::new(None),
            said: Mutex::new(Vec::new()),
            said_err: Mutex::new(Vec::new()),
            broker_unreachable: false,
            broker_state: ServiceState::NotInstalled,
            after_restart: Mutex::new(Vec::new()),
            service_runner: None,
            service_unreachable: false,
        }
    }

    /// A broker whose admin channel cannot be reached; `state` is what the OS says of it.
    pub fn unreachable_broker(state: ServiceState) -> FakeRoles {
        FakeRoles {
            broker_unreachable: true,
            broker_state: state,
            ..FakeRoles::new(true, 0)
        }
    }

    pub fn said_err(&self) -> Vec<String> {
        self.said_err.lock().unwrap().clone()
    }

    pub fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }

    pub fn broker_stops(&self) -> usize {
        self.broker_stops.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Roles for FakeRoles {
    async fn stop_runner(
        &self,
        name: &str,
        mode: DownMode,
        reason: Option<DrainReason>,
    ) -> Result<bool, String> {
        self.told.lock().unwrap().push((name.to_string(), mode));
        self.reasons.lock().unwrap().push(reason);
        if let Some(path) = self.lock_file.lock().unwrap().as_ref() {
            self.lock_seen.lock().unwrap().push(path.exists());
        }
        Ok(!self.service_unreachable)
    }

    async fn runner_service(&self, _name: &str) -> Option<Option<bool>> {
        self.service_runner
    }

    async fn running_jobs(&self) -> Option<Vec<String>> {
        self.jobs.lock().unwrap().clone()
    }

    fn say(&self, line: &str) {
        self.said.lock().unwrap().push(line.to_string());
    }

    async fn runner_answers(&self, _name: &str) -> bool {
        let left = self.answers_left.load(Ordering::SeqCst);
        if left == usize::MAX {
            return true;
        }
        if left == 0 {
            return false;
        }
        self.answers_left.store(left - 1, Ordering::SeqCst);
        true
    }

    async fn stop_hub(&self) -> Result<bool, String> {
        self.hub_stops.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    async fn stop_broker(&self) -> Result<Option<Vec<String>>, String> {
        self.broker_stops.fetch_add(1, Ordering::SeqCst);
        if self.broker_unreachable {
            return Ok(None);
        }
        match self.service {
            true => Ok(Some(vec!["runs as an OS service and stays".into()])),
            false => Ok(Some(Vec::new())),
        }
    }

    fn broker_is_service(&self) -> bool {
        self.service
    }

    fn say_err(&self, line: &str) {
        self.said_err.lock().unwrap().push(line.to_string());
    }

    async fn broker_service_state(&self) -> ServiceState {
        self.broker_state
    }

    async fn pool_after_restart(&self) -> Vec<PoolRunner> {
        self.after_restart.lock().unwrap().clone()
    }
}
