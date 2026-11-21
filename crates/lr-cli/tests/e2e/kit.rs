use crate::common::{self, TempDir};
pub use crate::helper::{Gate, Mode, Release};
use crate::support::*;
use lr_abstractions::{Liveness, ProcessInspector};
use lr_broker::BusEvent;
use lr_cli::commands::jobs::hub;
use lr_cli::commands::join_cmd::hub_join;
use lr_cli::commands::view::known_brokers;
use lr_cli::ipc_client::IpcClient;
use lr_cli::paths::Layout;
use lr_cli::roles::{start_broker, start_hub, start_runners, Running};
use lr_exec::OsProcessInspector;
use lr_proto::{AdminReply, AdminRequest, BrokerStatus, JobSummary, QueueItem};
use std::path::Path;
use tokio::sync::broadcast::Receiver;

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

    pub async fn stopped(mut self) {
        while let Some(r) = self.0.pop() {
            r.stopped().await;
        }
    }

    /// The bus events of the broker of a [`machine`] from now on.
    pub fn broker_events(&self) -> Receiver<BusEvent> {
        self.0[0]
            .broker_events()
            .expect("the broker of the machine")
    }

    /// Writes out the diagnostics of every role.
    pub fn flush(&self) {
        for r in &self.0 {
            r.flush();
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
    until_hub(&guest, "the joined hub reaches the broker", || {
        broker_available(&guest)
    })
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

/// A profile whose step is the waiting helper `name` of `gate`; `extra` adds lines such as
/// `mem_mb`.
pub fn held_profile(name: &str, gate: &Gate, timeout_s: u64, extra: &str) -> String {
    format!(
        "[profile.{name}]\ncommand = {}\ntimeout_s = {timeout_s}\n{extra}",
        toml_str(&gate.command(Mode::Wait, name))
    )
}

/// The `lan` and `remote` variants of `name` that run `command`, so the profile may go beyond
/// the machine of the client.
pub fn far_variants(name: &str, command: &str) -> String {
    ["lan", "remote"]
        .iter()
        .map(|zone| {
            format!(
                "\n[profile.{name}.zone.{zone}]\ncommand = {}\n",
                toml_str(command)
            )
        })
        .collect()
}

/// A held profile that may also run in the `lan` and `remote` zones.
pub fn held_far_profile(name: &str, gate: &Gate, timeout_s: u64, extra: &str) -> String {
    let command = gate.command(Mode::Wait, name);
    format!(
        "{}{}",
        held_profile(name, gate, timeout_s, extra),
        far_variants(name, &command)
    )
}

/// A profile that prints `text` and ends and may also run in the `lan` and `remote` zones.
pub fn echo_far_profile(name: &str, text: &str, timeout_s: u64, extra: &str) -> String {
    format!(
        "{}{}",
        echo_profile(name, text, timeout_s, extra),
        far_variants(name, &format!("echo {text}"))
    )
}

/// A profile that prints `text` and ends.
pub fn echo_profile(name: &str, text: &str, timeout_s: u64, extra: &str) -> String {
    format!("[profile.{name}]\ncommand = \"echo {text}\"\ntimeout_s = {timeout_s}\n{extra}")
}

pub fn alive(process: (u32, u64)) -> bool {
    OsProcessInspector.liveness(process.0, process.1) == Liveness::Alive
}

/// The queue entry of `job` at the local broker of `layout`, once it waits there.
pub async fn queued(layout: &Layout, job: &lr_core::JobId) -> QueueItem {
    until_hub(layout, &format!("job {job} queued"), || async {
        let v = status(layout).await?;
        v.queue.into_iter().find(|q| &q.summary.job == job)
    })
    .await
}

/// The history row of `job` among the runs of every client the broker of `layout` shows.
pub async fn row(layout: &Layout, job: &lr_core::JobId) -> JobSummary {
    until_hub(layout, &format!("job {job} in the history"), || async {
        try_history(layout, false)
            .await
            .ok()?
            .into_iter()
            .find(|r| &r.job == job && r.state.is_final())
    })
    .await
}
