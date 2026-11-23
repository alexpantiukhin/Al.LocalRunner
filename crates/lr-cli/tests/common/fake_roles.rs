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
    pub pool_hangs: bool,
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
            pool_hangs: false,
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
        if self.pool_hangs {
            let far = tokio::time::Instant::now() + std::time::Duration::from_secs(3600);
            tokio::time::sleep_until(far).await;
        }
        self.after_restart.lock().unwrap().clone()
    }
}

/// The role side of `config` commands: whether writing needs rights, which roles were reloaded
/// (running ones answer `reloaded`, `refuse` makes them refuse, `reload_error` makes the call
/// fail) and restarted, and a restart command named after the role.
pub struct FakeControl {
    pub rights: bool,
    pub running: bool,
    pub refuse: Option<String>,
    pub reload_error: Option<String>,
    reloads: Mutex<Vec<lr_cli::commands::down_cmd::Target>>,
    restarts: Mutex<Vec<lr_cli::commands::down_cmd::Target>>,
}

impl FakeControl {
    pub fn new(rights: bool, running: bool) -> FakeControl {
        FakeControl {
            rights,
            running,
            refuse: None,
            reload_error: None,
            reloads: Mutex::new(Vec::new()),
            restarts: Mutex::new(Vec::new()),
        }
    }

    pub fn reloads(&self) -> Vec<lr_cli::commands::down_cmd::Target> {
        self.reloads.lock().unwrap().clone()
    }

    pub fn restarts(&self) -> Vec<lr_cli::commands::down_cmd::Target> {
        self.restarts.lock().unwrap().clone()
    }
}

fn role_name(role: &lr_cli::commands::down_cmd::Target) -> String {
    use lr_cli::commands::down_cmd::Target;
    match role {
        Target::Runner(n) => n.clone(),
        Target::Hub => "hub".into(),
        Target::Broker => "broker".into(),
    }
}

#[async_trait(?Send)]
impl lr_cli::config_edit::RoleControl for FakeControl {
    fn needs_rights(&self, _path: &std::path::Path) -> bool {
        self.rights
    }

    fn clock(&self) -> &dyn lr_abstractions::Clock {
        &lr_exec::SystemClock
    }

    async fn reload(
        &self,
        _layout: &Layout,
        _target: &lr_cli::config_edit::Target,
        role: &lr_cli::commands::down_cmd::Target,
    ) -> Result<lr_cli::commands::reload_cmd::Outcome, String> {
        use lr_cli::commands::reload_cmd::Outcome;
        self.reloads.lock().unwrap().push(role.clone());
        if let Some(e) = &self.reload_error {
            return Err(e.clone());
        }
        if let Some(why) = &self.refuse {
            return Ok(Outcome::Refused(why.clone()));
        }
        Ok(match self.running {
            true => Outcome::Reloaded {
                restart_required: Vec::new(),
            },
            false => Outcome::NotRunning,
        })
    }

    fn restart_command(
        &self,
        _layout: &Layout,
        role: &lr_cli::commands::down_cmd::Target,
    ) -> String {
        format!("restart-command-of-{}", role_name(role))
    }

    async fn restart(
        &self,
        _layout: &Layout,
        role: &lr_cli::commands::down_cmd::Target,
    ) -> Result<i32, String> {
        self.restarts.lock().unwrap().push(role.clone());
        Ok(0)
    }
}

/// An elevator that records each call and gives `answer`; nothing is started.
pub struct FakeElevator {
    answer: Result<i32, String>,
    calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
}

impl FakeElevator {
    pub fn new(answer: Result<i32, String>) -> FakeElevator {
        FakeElevator {
            answer,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl lr_cli::config_edit::Elevator for FakeElevator {
    fn run_elevated(&self, exe: &std::path::Path, args: &[String]) -> Result<i32, String> {
        self.calls
            .lock()
            .unwrap()
            .push((exe.to_path_buf(), args.to_vec()));
        self.answer.clone()
    }
}
