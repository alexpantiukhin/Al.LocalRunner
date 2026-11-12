use crate::common::{self, TempDir};
use crate::helper::{helper_command, Mode};
use crate::support::*;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_cli::commands::jobs::hub;
use lr_cli::commands::join_cmd::hub_join;
use lr_cli::commands::view::known_brokers;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners, Running};
use lr_exec::{process_start, OsProcessInspector};
use lr_proto::{AdminReply, AdminRequest, BrokerStatus, JobSummary, QueueItem};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PLATFORM: &str = "e2e-remote";

/// The roles of one test machine, stopped in the reverse order of their start.
#[derive(Default)]
pub struct Roles(Vec<Running>);

impl Roles {
    pub fn push(&mut self, role: Running) {
        self.0.push(role);
    }

    /// The hub of a [`machine`], started right after its broker.
    pub fn take_hub(&mut self) -> Running {
        self.0.remove(1)
    }

    /// The role started last.
    pub fn take_last(&mut self) -> Running {
        self.0.pop().expect("a role")
    }

    /// The broker of a [`machine`], started first.
    pub fn take_broker(&mut self) -> Running {
        self.0.remove(0)
    }

    pub fn stop(mut self) {
        while let Some(r) = self.0.pop() {
            r.stop();
        }
    }
}

/// `runner.toml` of a test runner that also serves [`PLATFORM`].
pub fn platform_runner_toml() -> String {
    format!("platforms = [\"{PLATFORM}\"]\n{}", runner_toml())
}

/// Creates runner `name` of `layout` with `toml`.
pub fn add_runner_with(layout: &Layout, name: &str, toml: &str) {
    let dir = layout.runner_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("runner.toml"), toml).unwrap();
}

/// Broker, hub and the named runners (each with `toml`) of machine `name`, all online.
pub async fn machine(
    dir: &TempDir,
    name: &str,
    policy: &str,
    runners: &[&str],
    toml: &str,
) -> (Layout, Roles) {
    let layout = layout(dir, name, policy);
    let mut roles = Roles::default();
    roles.push(start_broker(&layout, false).await.expect("broker starts"));
    roles.push(start_hub(&layout, false).await.expect("hub starts"));
    for r in runners {
        add_runner_with(&layout, r, toml);
        let started = start_runners(&layout, Some(r), 1, None, false)
            .await
            .unwrap_or_else(|e| panic!("runner {r} starts: {e}"));
        roles.push(started);
        runner_online(&layout, r).await;
    }
    (layout, roles)
}

/// One administrative request on the broker's admin channel.
pub async fn admin(layout: &Layout, req: AdminRequest) -> AdminReply {
    let admin = IpcClient::connect_broker_admin(&layout.admin_ipc())
        .await
        .expect("admin channel");
    let reply: AdminReply = admin.call(&req).await.expect("admin reply");
    admin.close().await;
    reply
}

/// A join code: for a hub named `name` with `label`, or for `runners` runners.
pub async fn join_code(
    layout: &Layout,
    name: Option<&str>,
    label: Option<&str>,
    runners: Option<u32>,
) -> (String, String) {
    let req = AdminRequest::BrokerAdd {
        name: name.map(str::to_string),
        label: label.map(str::to_string),
        runners,
        address: None,
    };
    match admin(layout, req).await {
        AdminReply::JoinCode { address, code, .. } => (address, code),
        other => panic!("broker add answered {other:?}"),
    }
}

/// Machine `name` whose hub joined the broker of `main` as `broker` with a code for a client
/// `client` labelled `label`; the hub runs and reads the broker.
pub async fn joined_hub(
    dir: &TempDir,
    main: &Layout,
    name: &str,
    client: &str,
    label: Option<&str>,
) -> (Layout, Running) {
    let guest = layout(dir, name, "");
    let (address, code) = join_code(main, Some(client), label, None).await;
    assert_eq!(
        hub_join(&guest, &address, &code, Some("main".into())).await,
        Ok(0)
    );
    let hub = start_hub(&guest, false)
        .await
        .expect("the joined hub starts");
    eventually(
        "the joined hub reaches the broker",
        Duration::from_secs(60),
        || async { broker_available(&guest).await },
    )
    .await;
    (guest, hub)
}

/// The brokers the hub of `layout` knows, with their zones and availability.
pub async fn brokers(layout: &Layout) -> Vec<BrokerStatus> {
    let client = hub(layout).await.expect("hub channel");
    let known = known_brokers(&client).await.expect("brokers");
    client.close().await;
    known
}

/// The names the broker of `layout` answers to `RunnerList` or `ClientList`.
pub async fn names(layout: &Layout, req: AdminRequest) -> Vec<String> {
    match admin(layout, req).await {
        AdminReply::Names { names } => names,
        other => panic!("list answered {other:?}"),
    }
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// Pushes the committed `repo` to a bare `origin` next to it, so remote runs can fetch it.
pub fn push_origin(dir: &TempDir, repo: &Path) {
    common::git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
    let url = file_url(&dir.path().join("origin.git"));
    common::git(repo, &["remote", "add", "origin", &url]);
    common::git(repo, &["push", "-q", "-u", "origin", "HEAD"]);
}

/// A profile whose step is the waiting helper: it writes its pid to `<dir>/<name>.pid.child`
/// and ends when `gate` exists; `extra` adds lines such as `mem_mb`.
pub fn held_profile(name: &str, dir: &Path, gate: &Path, timeout_s: u64, extra: &str) -> String {
    let pids = pid_file(dir, name);
    format!(
        "[profile.{name}]\ncommand = {}\ntimeout_s = {timeout_s}\n{extra}",
        toml_str(&helper_command(Mode::Wait, &pids, gate))
    )
}

/// A profile that prints `text` and ends.
pub fn echo_profile(name: &str, text: &str, timeout_s: u64, extra: &str) -> String {
    format!("[profile.{name}]\ncommand = \"echo {text}\"\ntimeout_s = {timeout_s}\n{extra}")
}

pub fn pid_file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.pid"))
}

pub fn open(gate: &Path) {
    std::fs::write(gate, b"open").unwrap();
}

pub fn alive(pid: u32, start: u64) -> bool {
    OsProcessInspector.liveness(pid, start) == Liveness::Alive
}

/// The pid and start of the helper `role` of profile `name`, once it runs.
pub async fn started(dir: &Path, name: &str, role: &str) -> (u32, u64) {
    let pid = helper_pid(&pid_file(dir, name), role).await;
    (pid, process_start(pid).expect("the helper runs"))
}

/// Waits until every process of `tree` is gone.
pub async fn gone(what: &str, tree: &[(u32, u64)]) {
    eventually(what, Duration::from_secs(30), || async {
        tree.iter().all(|&(p, s)| !alive(p, s)).then_some(())
    })
    .await;
}

/// Opens the gates when the test ends, even by a panic, and waits until the watched
/// processes are gone, so none of them holds the temporary directory.
pub struct Release {
    gates: Vec<PathBuf>,
    watched: Vec<(u32, u64)>,
}

impl Release {
    pub fn new(gates: &[&Path]) -> Release {
        Release {
            gates: gates.iter().map(|g| g.to_path_buf()).collect(),
            watched: Vec::new(),
        }
    }

    pub fn watch(&mut self, process: (u32, u64)) {
        self.watched.push(process);
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        for gate in &self.gates {
            let _ = std::fs::write(gate, b"open");
        }
        let started = Instant::now();
        while self.watched.iter().any(|&(p, s)| alive(p, s))
            && started.elapsed() < Duration::from_secs(20)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The queue entry of `job` at the local broker of `layout`, once it waits there.
pub async fn queued(layout: &Layout, job: &lr_core::JobId) -> QueueItem {
    eventually(
        &format!("job {job} queued"),
        Duration::from_secs(60),
        || async {
            let v = status(layout).await?;
            v.queue.into_iter().find(|q| &q.summary.job == job)
        },
    )
    .await
}

/// The history row of `job` among the runs of every client the broker of `layout` shows.
pub async fn row(layout: &Layout, job: &lr_core::JobId) -> JobSummary {
    eventually(
        &format!("job {job} in the history"),
        Duration::from_secs(60),
        || async {
            try_history(layout, false)
                .await
                .ok()?
                .into_iter()
                .find(|r| &r.job == job && r.state.is_final())
        },
    )
    .await
}
