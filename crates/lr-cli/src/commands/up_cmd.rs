//! `up [role]` (spec 11.1): starts the broker, the hub and the runners of this installation as
//! separate background processes. A role that answers on its local IPC already runs and is
//! not started again; the broker counts as running when its IPC or its admin channel answers,
//! which also finds a broker that runs as an OS service.

use super::service_cmd::chosen_roles;
use super::RoleArg;
use crate::init::NodeRole;
use crate::ipc_client::IpcClient;
use crate::paths::Layout;
use lr_abstractions::Dialer;
use lr_proto::IpcChannel;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const STARTED_WITHIN: Duration = Duration::from_secs(15);

/// The roles of this installation that answer on their local IPC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunningRoles {
    /// The hub of this OS user.
    pub hub: bool,
    /// The broker.
    pub broker: bool,
    /// Runners by name.
    pub runners: BTreeSet<String>,
}

async fn answers(address: &str, channel: IpcChannel) -> bool {
    match IpcClient::connect(address, channel).await {
        Ok(c) => {
            c.close().await;
            true
        }
        Err(_) => false,
    }
}

/// Whether a broker of `layout` answers: its IPC (a real connect, so a socket file left behind
/// does not count) or its admin channel.
pub async fn broker_answers(layout: &Layout) -> bool {
    if let Ok(servers) = crate::os_service::broker_servers() {
        let dialer = lr_net::IpcDialer::new(servers);
        if let Ok(t) = dialer.dial(&layout.broker_ipc(), None).await {
            t.close().await;
            return true;
        }
    }
    match IpcClient::connect_broker_admin(&layout.admin_ipc()).await {
        Ok(c) => {
            c.close().await;
            true
        }
        Err(_) => false,
    }
}

/// Which roles of `layout` run now.
pub async fn running_roles(layout: &Layout) -> RunningRoles {
    let mut out = RunningRoles {
        broker: broker_answers(layout).await,
        ..RunningRoles::default()
    };
    let Ok(principal) = lr_net::current_os_principal() else {
        return out;
    };
    out.hub = answers(&layout.hub_ipc(&principal), IpcChannel::Cli).await;
    for name in layout.runner_names() {
        if answers(
            &layout.runner_ipc(&principal, &name),
            IpcChannel::RunnerControl,
        )
        .await
        {
            out.runners.insert(name);
        }
    }
    out
}

fn dirs(layout: &Layout) -> Vec<String> {
    let mut out = vec![
        "--state-dir".to_string(),
        layout.root.to_string_lossy().into_owned(),
    ];
    if layout.config != layout.root {
        out.extend([
            "--config-dir".to_string(),
            layout.config.to_string_lossy().into_owned(),
        ]);
    }
    out
}

/// The command lines (without the binary) that start the roles of `roles` that do not run:
/// the broker first, then the hub, then one process per runner of the installation (the default
/// runner when there is none yet).
pub fn up_plan(layout: &Layout, roles: &[NodeRole], running: &RunningRoles) -> Vec<Vec<String>> {
    let line = |words: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        v.push("--background".into());
        v.extend(dirs(layout));
        v
    };
    let mut out = Vec::new();
    if roles.contains(&NodeRole::Broker) && !running.broker {
        out.push(line(&["broker", "run"]));
    }
    if roles.contains(&NodeRole::Hub) && !running.hub {
        out.push(line(&["hub", "run"]));
    }
    if roles.contains(&NodeRole::Runner) {
        let names = layout.runner_names();
        if names.is_empty() && running.runners.is_empty() {
            out.push(line(&["runner", "run"]));
        }
        for n in names.iter().filter(|n| !running.runners.contains(*n)) {
            out.push(line(&["runner", "run", "--name", n]));
        }
    }
    out
}

#[cfg(windows)]
fn keep_std_handles_from_children() {
    use windows_sys::Win32::Foundation::{
        SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(id) };
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

fn spawn(exe: &Path, args: &[String]) -> std::io::Result<u32> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd.spawn()?.id())
}

fn missing(plan: &[Vec<String>], now: &RunningRoles) -> Vec<String> {
    let mut out = Vec::new();
    for argv in plan {
        let ok = match argv.first().map(String::as_str) {
            Some("broker") => now.broker,
            Some("hub") => now.hub,
            _ => match argv.iter().position(|a| a == "--name") {
                Some(i) => argv.get(i + 1).is_some_and(|n| now.runners.contains(n)),
                None => !now.runners.is_empty(),
            },
        };
        if !ok {
            out.push(argv[..2].join(" "));
        }
    }
    out
}

/// The roles `up` may start: a broker installed as an OS service is left to the service
/// manager, so a stopped service never turns into a broker of the user's directory.
pub fn startable(roles: Vec<NodeRole>, broker_is_service: bool) -> Vec<NodeRole> {
    roles
        .into_iter()
        .filter(|r| !(broker_is_service && *r == NodeRole::Broker))
        .collect()
}

/// `up [role]`.
pub async fn up(layout: &Layout, role: Option<RoleArg>) -> Result<i32, String> {
    let chosen = chosen_roles(layout, role)?;
    let service = chosen.contains(&NodeRole::Broker) && layout.broker_is_service();
    let roles = startable(chosen, service);
    let before = running_roles(layout).await;
    let mut code = 0;
    if service {
        match before.broker {
            true => println!("broker: running as an OS service"),
            false => {
                eprintln!(
                    "broker: installed as an OS service and not running; `up` does not start \
                     it: start it with the OS service manager"
                );
                code = 1;
            }
        }
    }
    if roles.contains(&NodeRole::Broker) && before.broker {
        println!("broker: already running");
    }
    if roles.contains(&NodeRole::Hub) && before.hub {
        println!("hub: already running");
    }
    for r in &before.runners {
        println!("runner {r}: already running");
    }
    let plan = up_plan(layout, &roles, &before);
    if plan.is_empty() {
        return Ok(code);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    #[cfg(windows)]
    keep_std_handles_from_children();
    for argv in &plan {
        match spawn(&exe, argv) {
            Ok(pid) => println!("{}: started (pid {pid})", argv[..2].join(" ")),
            Err(e) => {
                eprintln!("{}: {e}", argv[..2].join(" "));
                code = 1;
            }
        }
    }
    let start = tokio::time::Instant::now();
    let late = loop {
        let late = missing(&plan, &running_roles(layout).await);
        if late.is_empty() || start.elapsed() >= STARTED_WITHIN {
            break late;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    for l in late {
        eprintln!(
            "{l}: does not answer yet; see logs/ in {} or run `localrunner {l}` in the foreground",
            layout.root.display()
        );
        code = 1;
    }
    Ok(code)
}
