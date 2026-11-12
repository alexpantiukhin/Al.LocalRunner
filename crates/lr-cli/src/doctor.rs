//! `doctor` (spec 10.2): checks this node and prints what is wrong — OS, privileges,
//! directories, tree containment (cgroup v2 on Linux), `policy.toml` and its ports, runners
//! with their programs, and the reachability and certificate of every joined broker.

use crate::build::{BROKER_FILE, KEY_FILE};
use crate::paths::Layout;
use crate::runner_config::RunnerConfig;
use lr_abstractions::{Dialer, HubStore};
use lr_net::TlsDialer;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

const DIAL_LIMIT: Duration = Duration::from_secs(5);

/// The verdict of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Fine.
    Ok,
    /// Works, with a caveat.
    Warn,
    /// Broken.
    Fail,
}

/// One check and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// The verdict.
    pub status: Status,
    /// What was found.
    pub detail: String,
}

fn check(name: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

fn writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn basics(layout: &Layout, elevated: bool, out: &mut Vec<Check>) {
    out.push(check(
        "os",
        Status::Ok,
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    ));
    out.push(if elevated {
        check(
            "privileges",
            Status::Warn,
            "running as root or administrator: runners refuse to start without allow_elevated",
        )
    } else {
        check("privileges", Status::Ok, "ordinary user")
    });
    out.push(match writable(&layout.root) {
        Ok(()) => check(
            "state directory",
            Status::Ok,
            layout.root.display().to_string(),
        ),
        Err(e) => check(
            "state directory",
            Status::Fail,
            format!("{}: {e}", layout.root.display()),
        ),
    });
    use lr_exec::Containment;
    out.push(match lr_exec::containment() {
        Containment::JobObject => check("process tree", Status::Ok, "Job Object"),
        Containment::CgroupScope => check(
            "process tree",
            Status::Ok,
            "cgroup v2 scope (systemd-run --user --scope)",
        ),
        Containment::ProcessGroup => check(
            "process tree",
            Status::Warn,
            "process group only: detached descendants may survive (Linux: no cgroup v2 user scope)",
        ),
    });
}

fn policy(layout: &Layout, out: &mut Vec<Check>) {
    let path = layout.config_file("policy.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let file = match lr_broker::policy::load(&text) {
        Ok(f) => f,
        Err(e) => {
            out.push(check("policy.toml", Status::Fail, e.to_string()));
            return;
        }
    };
    out.push(check("policy.toml", Status::Ok, path.display().to_string()));
    let in_container = crate::host::in_container();
    let addresses = crate::broker_admin::listen_addresses(&file.listen.addresses, in_container);
    for a in addresses.unwrap_or_default() {
        let addr = a.socket_addr();
        if addr.port() == 0 {
            continue;
        }
        out.push(match std::net::TcpListener::bind(addr) {
            Ok(_) => check(format!("port {addr}"), Status::Ok, "free"),
            Err(e) => check(
                format!("port {addr}"),
                Status::Warn,
                format!("{e} (the broker itself, or another program)"),
            ),
        });
    }
}

/// The risk of spec 3.12 when the broker of `layout` runs under this OS user, not as an OS
/// service, next to a runner of the same user (`with_runner`) while its policy (the default
/// one without `policy.toml`) admits clients other than the owner (a `[listen]` address or a
/// `[[local_user]]`) and lets some client run any command: the job of such a client then acts
/// as the pool owner (admin channel, broker key and databases). `None` when that is not the
/// case or the policy is unreadable.
pub fn same_user_risk(layout: &Layout, with_runner: bool) -> Option<String> {
    use lr_broker::policy::CommandMode;
    if !with_runner || layout.broker_is_service() {
        return None;
    }
    let text = match std::fs::read_to_string(layout.config_file("policy.toml")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return None,
    };
    let file = lr_broker::policy::load(&text).ok()?;
    if file.listen.addresses.is_empty() && file.local_users.is_empty() {
        return None;
    }
    let any = file.commands.default == CommandMode::Any
        || file
            .commands
            .client
            .iter()
            .any(|c| c.mode == CommandMode::Any);
    any.then(|| {
        [
            "the broker and a runner run under the same OS user and the policy lets clients run",
            "any command: a job of another client can use the broker admin channel, its key and",
            "its databases (spec 3.12); run the runner under its own OS user, or the broker as an",
            "OS service, or set [commands] default = \"named\"",
        ]
        .join(" ")
    })
}

fn same_user(layout: &Layout, out: &mut Vec<Check>) {
    if let Some(risk) = same_user_risk(layout, !layout.runner_names().is_empty()) {
        out.push(check("broker and runner user", Status::Warn, risk));
    }
}

fn runners(layout: &Layout, out: &mut Vec<Check>) {
    let host = crate::host::resources();
    for name in layout.runner_names() {
        let dir = layout.runner_dir(&name);
        let label = format!("runner {name}");
        let text = crate::build::runner_config_path(layout, &name, None)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        match RunnerConfig::parse(&text, Some(&name), &name, dir, host) {
            Err(e) => out.push(check(label, Status::Fail, e.to_string())),
            Ok(mut cfg) => {
                let problems = cfg.check_programs();
                if problems.is_empty() {
                    out.push(check(label.clone(), Status::Ok, "runner.toml and programs"));
                }
                for p in problems {
                    out.push(check(label.clone(), Status::Warn, p));
                }
            }
        }
    }
}

async fn reach(name: String, key: &Path, address: &str, fingerprint: &str) -> Check {
    let key = match lr_net::ClientKey::load(key) {
        Ok(k) => std::sync::Arc::new(k),
        Err(e) => return check(name, Status::Fail, format!("{}: {e}", key.display())),
    };
    let dialer = TlsDialer::new(key);
    match tokio::time::timeout(DIAL_LIMIT, dialer.dial(address, Some(fingerprint))).await {
        Ok(Ok(t)) => {
            t.close().await;
            check(
                name,
                Status::Ok,
                format!("{address} reachable, certificate fingerprint matches, key accepted"),
            )
        }
        Ok(Err(e)) => check(name, Status::Fail, format!("{address}: {e}")),
        Err(_) => check(name, Status::Fail, format!("{address}: no answer in 5 s")),
    }
}

#[derive(serde::Deserialize)]
struct BrokerFile {
    address: String,
    fingerprint: Option<String>,
}

async fn brokers(layout: &Layout, out: &mut Vec<Check>) {
    for name in layout.runner_names() {
        let dir = layout.runner_dir(&name);
        let Ok(text) = std::fs::read_to_string(dir.join(BROKER_FILE)) else {
            continue;
        };
        let label = format!("runner {name} broker");
        match toml::from_str::<BrokerFile>(&text) {
            Ok(BrokerFile {
                address,
                fingerprint: Some(fp),
            }) => out.push(reach(label, &dir.join(KEY_FILE), &address, &fp).await),
            _ => out.push(check(
                label,
                Status::Fail,
                format!("{BROKER_FILE} is unreadable"),
            )),
        }
    }
    let db = layout.root.join("hub.db");
    if !db.exists() {
        return;
    }
    let links = match lr_store::SqliteHubStore::open(&db) {
        Ok(store) => store.brokers().await.unwrap_or_default(),
        Err(e) => {
            out.push(check("hub.db", Status::Fail, e.to_string()));
            return;
        }
    };
    for link in links {
        let label = format!("broker {}", link.name);
        let key = layout.root.join(KEY_FILE);
        out.push(reach(label, &key, &link.address, &link.fingerprint).await);
    }
}

/// Runs every check of this node.
pub async fn checks(layout: &Layout) -> Vec<Check> {
    checks_with(layout, crate::host::is_elevated()).await
}

/// Runs every check of this node for a process whose rights are `elevated`.
pub async fn checks_with(layout: &Layout, elevated: bool) -> Vec<Check> {
    let mut out = Vec::new();
    basics(layout, elevated, &mut out);
    policy(layout, &mut out);
    runners(layout, &mut out);
    same_user(layout, &mut out);
    brokers(layout, &mut out).await;
    out
}

/// `doctor [--json]`: exit code 1 when a check failed.
pub async fn run(layout: &Layout, json: bool) -> Result<i32, String> {
    let all = checks(layout).await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?
        );
    } else {
        for c in &all {
            let tag = match c.status {
                Status::Ok => "ok  ",
                Status::Warn => "warn",
                Status::Fail => "FAIL",
            };
            println!("{tag} {}: {}", c.name, c.detail);
        }
    }
    Ok(i32::from(all.iter().any(|c| c.status == Status::Fail)))
}
