use async_trait::async_trait;
use lr_cli::commands::down_cmd::Roles;
use lr_cli::paths::Layout;
use lr_proto::DownMode;
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
    pub hub_stops: AtomicUsize,
    pub broker_stops: AtomicUsize,
}

impl FakeRoles {
    pub fn new(service: bool, answers_left: usize) -> FakeRoles {
        FakeRoles {
            service,
            answers_left: AtomicUsize::new(answers_left),
            told: Mutex::new(Vec::new()),
            hub_stops: AtomicUsize::new(0),
            broker_stops: AtomicUsize::new(0),
        }
    }

    pub fn broker_stops(&self) -> usize {
        self.broker_stops.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Roles for FakeRoles {
    async fn stop_runner(&self, name: &str, mode: DownMode) -> Result<bool, String> {
        self.told.lock().unwrap().push((name.to_string(), mode));
        Ok(true)
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
        match self.service {
            true => Ok(Some(vec!["runs as an OS service and stays".into()])),
            false => Ok(Some(Vec::new())),
        }
    }

    fn broker_is_service(&self) -> bool {
        self.service
    }
}
