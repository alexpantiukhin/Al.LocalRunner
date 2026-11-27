//! `doctor` (spec 10.2): checks this node and prints what is wrong — OS, privileges,
//! directories, tree containment (cgroup v2 on Linux), `policy.toml` and its ports, runners
//! with their programs, and the reachability and certificate of every joined broker.

use crate::build::{BROKER_FILE, KEY_FILE};
use crate::paths::Layout;
use crate::render::{Level, Out, Report};
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
        Containment::DelegatedCgroup => check(
            "process tree",
            Status::Ok,
            "cgroup v2 subtree delegated to this process (Delegate=yes)",
        ),
        Containment::ProcessGroup => check(
            "process tree",
            Status::Warn,
            "process group only: this check ran from your shell, not from the runner service, so \
             it says nothing about the service (see the `runner <name> memory limit` line and \
             `enforcement` in `localrunner runners`); for a process started like this, detached \
             descendants may survive and `limit` runs as `warn` (Linux: no cgroup v2 user scope \
             and no delegated subtree with the memory controller)",
        ),
    });
}

/// The files of a broker that ran in the state directory `root`: its database with the journal
/// files, certificate and key; only those that exist.
pub fn stale_broker_files(root: &Path) -> Vec<std::path::PathBuf> {
    [
        "broker.db",
        "broker.db-wal",
        "broker.db-shm",
        "broker-cert.pem",
        "broker-key.pem",
    ]
    .iter()
    .map(|n| root.join(n))
    .filter(|p| p.exists())
    .collect()
}

/// The warning for a broker database left in the state directory of a user while the broker
/// runs as the OS service: nothing uses it, and `up broker --state-dir` would start a second
/// broker on it.
pub fn stale_broker_db(
    root: &Path,
    broker_is_service: bool,
    root_is_service: bool,
) -> Option<Check> {
    let db = root.join("broker.db");
    (broker_is_service && !root_is_service && db.exists()).then(|| {
        check(
            "broker.db",
            Status::Warn,
            format!(
                "{} is left over: the broker runs as a service with its own state; nothing uses this database, remove it with its -wal/-shm files, certificate and key once you do not need the old history",
                db.display()
            ),
        )
    })
}

async fn policy(layout: &Layout, out: &mut Vec<Check>) {
    out.extend(stale_broker_db(
        &layout.root,
        layout.broker_is_service(),
        layout.is_service(),
    ));
    match crate::commands::admin_cmd::broker_policy(layout).await {
        Some(Ok(found)) => {
            let (path, warnings) = (found.path, found.warnings);
            out.push(check(
                "policy.toml",
                Status::Ok,
                format!("{path} (the file the running broker uses)"),
            ));
            for w in warnings {
                out.push(check("policy.toml", Status::Warn, w));
            }
            return;
        }
        Some(Err(e)) => {
            out.push(check("policy.toml", Status::Fail, e));
            return;
        }
        None => {}
    }
    if let Some(note) = crate::commands::admin_cmd::service_policy_note(layout) {
        if crate::commands::admin_cmd::service_running(layout).await {
            out.push(check(
                "policy.toml",
                Status::Ok,
                crate::commands::admin_cmd::service_running_note(),
            ));
            return;
        }
        out.push(check("policy.toml", Status::Warn, note));
    }
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

fn names_another_user(entry: &lr_broker::policy::LocalUserEntry, owner: Option<&str>) -> bool {
    let Some(owner) = owner else {
        return true;
    };
    let users = lr_net::LocalUsers::new(vec![lr_net::LocalUser {
        name: entry.name.clone(),
        label: entry.label.clone(),
    }]);
    let me = lr_abstractions::Peer {
        endpoint: lr_core::Endpoint::Ipc {
            os_user: owner.to_string(),
        },
        key: None,
        access: lr_abstractions::PeerAccess::Local,
    };
    users.admit(&me).is_err()
}

static INSTALLATION_OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Names the OS user the installation belongs to for this process, when the process runs under
/// another account (a runner service under its Windows virtual account): [`same_user_risk`]
/// then compares the policy's users with that user instead of the process's own.
pub fn set_installation_owner(user: String) {
    let _ = INSTALLATION_OWNER.set(user);
}

/// The risk of spec 3.12 when the broker of `layout` runs under this OS user, not as an OS
/// service, next to a runner of the same user (`with_runner`) while its policy (the default
/// one without `policy.toml`) admits clients other than the owner (a `[listen]` address or a
/// `[[local_user]]` naming another OS user) and lets some client run any command: the job of
/// such a client then acts as the pool owner (admin channel, broker key and databases). `None`
/// when that is not the case or the policy is unreadable.
pub fn same_user_risk(layout: &Layout, with_runner: bool) -> Option<String> {
    same_user_risk_for(layout, with_runner, None)
}

/// [`same_user_risk`] for the installation of OS user `owner`; `None` means the owner set by
/// [`set_installation_owner`], else the user of this process. An elevated run (`sudo`, another
/// administrator) names the installing user, since its own user is not the owner.
pub fn same_user_risk_for(
    layout: &Layout,
    with_runner: bool,
    owner: Option<&str>,
) -> Option<String> {
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
    let owner = owner
        .map(str::to_string)
        .or_else(|| INSTALLATION_OWNER.get().cloned())
        .or_else(|| lr_net::current_os_user().ok());
    let others = file
        .local_users
        .iter()
        .any(|u| names_another_user(u, owner.as_deref()));
    if file.listen.addresses.is_empty() && !others {
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

/// Whether a runner of `layout` may start next to the broker of its OS user (spec 3.12): with
/// the risk of [`same_user_risk`] it is refused unless its `runner.toml` sets
/// `allow_same_user_as_broker = true` (`allowed`), which keeps the risk as a warning. `Ok(None)`
/// when there is no risk.
pub fn same_user_verdict(layout: &Layout, allowed: bool) -> Result<Option<String>, String> {
    same_user_verdict_for(layout, allowed, None)
}

/// [`same_user_verdict`] for the installation of OS user `owner` (see [`same_user_risk_for`]).
pub fn same_user_verdict_for(
    layout: &Layout,
    allowed: bool,
    owner: Option<&str>,
) -> Result<Option<String>, String> {
    match same_user_risk_for(layout, true, owner) {
        None => Ok(None),
        Some(risk) if allowed => Ok(Some(risk)),
        Some(risk) => Err(format!(
            "the runner refuses to start: {risk}; or set allow_same_user_as_broker = true in \
             runner.toml to accept the risk"
        )),
    }
}

/// `allow_same_user_as_broker` of the `runner.toml` runner `name` of `layout` uses; `false`
/// when the file is absent, unreadable or does not set it.
pub fn runner_allows_same_user(layout: &Layout, name: &str) -> bool {
    crate::build::runner_config_path(layout, name, None)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|text| crate::runner_config::allows_same_user_as_broker(&text))
}

fn same_user(layout: &Layout, out: &mut Vec<Check>) {
    for name in layout.runner_names() {
        let label = format!("runner {name} and broker user");
        match same_user_verdict(layout, runner_allows_same_user(layout, &name)) {
            Ok(None) => {}
            Ok(Some(risk)) => out.push(check(
                label,
                Status::Warn,
                format!("{risk} (allow_same_user_as_broker = true accepts it)"),
            )),
            Err(refusal) => out.push(check(label, Status::Fail, refusal)),
        }
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
                let env_notes = cfg.env_notes(cfg!(windows), |n| std::env::var(n).ok());
                if problems.is_empty() && cfg.warnings.is_empty() && env_notes.is_empty() {
                    out.push(check(label.clone(), Status::Ok, "runner.toml and programs"));
                }
                for w in &cfg.warnings {
                    out.push(check(
                        label.clone(),
                        Status::Warn,
                        format!("runner.toml: {w}"),
                    ));
                }
                for n in env_notes {
                    out.push(check(label.clone(), Status::Warn, n));
                }
                for p in problems {
                    out.push(check(label.clone(), Status::Warn, p));
                }
            }
        }
    }
}

/// How runner autostart is set up for one runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartFound {
    /// The runner's OS service: `None` when it has none, else whether it runs (`None` when the
    /// service manager did not say).
    pub service: Option<Option<bool>>,
    /// A login entry starts it (a scheduler task, a systemd user unit, a LaunchAgent).
    pub login: bool,
    /// Linux: `Linger` of the user (`loginctl show-user -p Linger`), `None` when unknown.
    pub linger: Option<bool>,
    /// The runner reaches its broker over the user's local channel and this OS gives such a
    /// runner no service (a Windows service account cannot use that channel): it has to be
    /// joined to the broker with its own key first.
    pub local_channel: bool,
}

/// `Linger` from `loginctl show-user -p Linger` output: `None` when the output has no such line.
pub fn parse_linger(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("Linger=")?;
        Some(value.trim() == "yes")
    })
}

/// The `autostart` check of runner `name`: `ok` when it survives logout and starts after a
/// reboot (an OS service, or a Linux user unit with lingering), `warn` for a login autostart
/// only, a stopped service, or a service and a login entry at once; `None` without autostart.
pub fn autostart_check(name: &str, found: &AutostartFound) -> Option<Check> {
    let label = format!("runner {name} autostart");
    let detail = match (found.service, found.login) {
        (Some(_), true) => (
            Status::Warn,
            format!(
                "an OS service and a login autostart both start it: remove the login one with \
                 `localrunner autostart off runner {name}`"
            ),
        ),
        (Some(Some(false)), false) => (
            Status::Warn,
            format!(
                "the OS service is installed but not running; its state: {}",
                crate::os_service::state::named_status_command(
                    crate::os_service::Os::current(),
                    &crate::os_service::runner_service::service_name(
                        crate::os_service::Os::current(),
                        name
                    )
                )
            ),
        ),
        (Some(_), false) => (
            Status::Ok,
            "OS service: survives logout and starts after a reboot".to_string(),
        ),
        (None, true) if found.linger == Some(true) => (
            Status::Ok,
            "user unit with lingering (loginctl enable-linger): survives logout and starts after a reboot"
                .to_string(),
        ),
        (None, true) if found.local_channel => (
            Status::Warn,
            format!(
                "starts at login only: it stops at logout and does not start after a reboot \
                 until someone logs in; it uses the local channel of this user, which a service \
                 cannot use, so `autostart on runner {name} --service` would be refused: {}",
                crate::os_service::runner_service::join_steps(name)
            ),
        ),
        (None, true) => (
            Status::Warn,
            format!(
                "starts at login only: it stops at logout and does not start after a reboot \
                 until someone logs in; to keep it running: `localrunner autostart on runner \
                 {name} --service` (administrator)"
            ),
        ),
        (None, false) => return None,
    };
    Some(check(label, detail.0, detail.1))
}

/// The `limit` check of runner `name` installed as a Linux system service whose unit reads
/// `unit`: `ok` when the unit delegates its cgroup subtree (`Delegate=yes`); whether the
/// runner can use it (the memory controller, `cgroup.kill`, a writable subtree) is decided when
/// it starts and shown as its `enforcement`. `warn` otherwise, because a system service has no
/// user manager and `limit` then runs as `warn`.
pub fn delegation_check(name: &str, unit: &str) -> Check {
    let label = format!("runner {name} memory limit");
    if unit.lines().any(|l| l.trim() == "Delegate=yes") {
        return check(
            label,
            Status::Ok,
            "the service unit has Delegate=yes; whether the runner holds job memory in it is \
             checked when the runner starts: `enforcement os` in `localrunner runners` means \
             yes, `watcher` means `limit` runs as `warn` (needs the memory controller and Linux \
             5.14+ for cgroup.kill)",
        );
    }
    check(
        label,
        Status::Warn,
        format!(
            "the service unit has no Delegate=yes and a system service has no user manager, so \
             `limit` runs as `warn` (enforcement watcher); reinstall the unit: `sudo localrunner \
             autostart off runner {name} --service`, then `sudo localrunner autostart on runner \
             {name} --service`"
        ),
    )
}

fn output_of(program: &str, args: &[&str]) -> Option<std::process::Output> {
    let path = crate::os_service::programs::program_path(program).ok()?;
    std::process::Command::new(path).args(args).output().ok()
}

fn login_entry(os: crate::os_service::Os, name: &str) -> bool {
    lr_net::current_os_user()
        .is_ok_and(|user| crate::os_service::runner_service::login_entry(os, &user, name))
}

fn linger(os: crate::os_service::Os) -> Option<bool> {
    if os != crate::os_service::Os::Linux {
        return None;
    }
    let user = lr_net::current_os_user().ok()?;
    let out = output_of("loginctl", &["show-user", "-p", "Linger", &user])?;
    parse_linger(&String::from_utf8_lossy(&out.stdout))
}

fn autostart(layout: &Layout, out: &mut Vec<Check>) {
    use crate::os_service::{runner_service, Os};
    let os = Os::current();
    for name in layout.runner_names() {
        let service =
            runner_service::installed(os, &name).then(|| runner_service::running(os, &name));
        let login = login_entry(os, &name);
        let joined = layout.runner_dir(&name).join(BROKER_FILE).is_file();
        let found = AutostartFound {
            local_channel: runner_service::local_runner_refusal(os, &name, joined).is_some(),
            service,
            login,
            linger: if login { linger(os) } else { None },
        };
        out.extend(autostart_check(&name, &found));
        if os == Os::Linux && found.service.is_some() {
            let unit = runner_service::definition_path(os, &name)
                .and_then(|p| std::fs::read_to_string(p).ok());
            out.extend(unit.map(|u| delegation_check(&name, &u)));
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
        let os = crate::os_service::Os::current();
        if os == crate::os_service::Os::Windows
            && crate::os_service::runner_service::installed(os, &name)
        {
            out.push(check(
                label,
                Status::Ok,
                "not checked: the runner runs as an OS service, which owns its key",
            ));
            continue;
        }
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

/// One warning per broker whose registered runners run another version than `mine` (or an
/// unknown one); nothing when all agree.
pub fn runner_version_checks(
    per: &[(String, Vec<lr_proto::RunnerInfo>)],
    mine: &str,
) -> Vec<Check> {
    per.iter()
        .filter_map(|(broker, runners)| {
            let off: Vec<String> = runners
                .iter()
                .filter(|r| r.caps.version.as_deref() != Some(mine))
                .map(|r| {
                    format!(
                        "{} ({})",
                        r.name,
                        r.caps.version.as_deref().unwrap_or("unknown")
                    )
                })
                .collect();
            (!off.is_empty()).then(|| {
                check(
                    format!("broker {broker} runners"),
                    Status::Warn,
                    format!(
                        "runners not on version {mine}: {}; run `localrunner upgrade` on their machines",
                        off.join(", ")
                    ),
                )
            })
        })
        .collect()
}

/// One warning per runner that a broker lists with declared programs it did not find, for the
/// runners of other machines too; a runner named in `own` is left out, its `runner.toml` is
/// checked by `runners`. The runner stays online and keeps taking the orders it fits.
pub fn runner_program_checks(
    per: &[(String, Vec<lr_proto::RunnerInfo>)],
    own: &[String],
) -> Vec<Check> {
    per.iter()
        .flat_map(|(broker, runners)| {
            runners
                .iter()
                .filter(|r| !r.caps.programs_missing.is_empty())
                .filter(|r| {
                    !own.iter()
                        .any(|n| crate::commands::upgrade_pool::is_own_name(&r.name, n))
                })
                .map(move |r| {
                    check(
                        format!("broker {broker} runner {}", r.name),
                        Status::Warn,
                        crate::commands::read_cmd::missing_programs_text(&r.caps.programs_missing),
                    )
                })
        })
        .collect()
}

async fn runner_versions(layout: &Layout, out: &mut Vec<Check>) {
    let Ok(client) = crate::commands::jobs::hub(layout).await else {
        return;
    };
    if let Ok((per, _)) = crate::commands::read_cmd::fetch_runners(&client, None).await {
        out.extend(runner_version_checks(&per, env!("CARGO_PKG_VERSION")));
        out.extend(runner_program_checks(&per, &layout.runner_names()));
    }
    client.close().await;
}

/// Runs every check of this node.
pub async fn checks(layout: &Layout) -> Vec<Check> {
    checks_with(layout, crate::host::is_elevated()).await
}

/// Runs every check of this node for a process whose rights are `elevated`.
pub async fn checks_with(layout: &Layout, elevated: bool) -> Vec<Check> {
    let mut out = Vec::new();
    basics(layout, elevated, &mut out);
    policy(layout, &mut out).await;
    runners(layout, &mut out);
    same_user(layout, &mut out);
    autostart(layout, &mut out);
    brokers(layout, &mut out).await;
    runner_versions(layout, &mut out).await;
    out.extend(crate::link_report::checks(layout).await);
    for line in crate::config_drift::lines(layout).await {
        out.push(check(
            "runner settings file".to_string(),
            Status::Warn,
            line,
        ));
    }
    out
}

/// The report of `doctor`: a marked line per check, then the counts.
pub fn report_lines(all: &[Check], out: &Out) -> Vec<String> {
    let mut report = Report::new();
    for c in all {
        let level = match c.status {
            Status::Ok => Level::Ok,
            Status::Warn => Level::Warn,
            Status::Fail => Level::Fail,
        };
        report.line(level, format!("{}: {}", c.name, c.detail));
    }
    report.lines(out)
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
        for line in report_lines(&all, &Out::detect()) {
            println!("{line}");
        }
    }
    Ok(i32::from(all.iter().any(|c| c.status == Status::Fail)))
}
