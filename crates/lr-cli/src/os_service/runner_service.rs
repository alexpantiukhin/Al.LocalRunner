//! A runner as an OS service (spec 3.12, 10, 10.3): it starts after a reboot without anyone
//! logging in and keeps running after logout. Windows: a service under its own virtual account
//! `NT SERVICE\AlLocalRunner-<name>`; Linux: a system unit under the installing user; macOS: a
//! LaunchDaemon under the installing user. Never under root or `SYSTEM`; installing and removing
//! it needs administrator rights.

use super::autostart::plist;
use super::quote::{systemd_command_line, windows_command_line, xml};
use super::{Action, Os, Tolerate};
use std::path::{Path, PathBuf};

/// What `autostart on|off runner <name> --service` needs.
#[derive(Debug, Clone)]
pub struct RunnerServiceInputs {
    /// Target OS.
    pub os: Os,
    /// The binary the service starts.
    pub exe: PathBuf,
    /// The runner.
    pub name: String,
    /// State directory of the installation the runner belongs to.
    pub state_dir: PathBuf,
    /// Configuration directory, when it is not the state directory.
    pub config_dir: Option<PathBuf>,
    /// The OS user who installs: the account of the service on Linux and macOS.
    pub user: String,
}

/// The privileges the Windows service keeps: no impersonation, so a job of the runner cannot
/// borrow another account's token.
pub const WINDOWS_PRIVILEGES: &str = "SeChangeNotifyPrivilege/SeCreateGlobalPrivilege";

const DESKTOP_MARKERS: [&str; 7] = [
    "emulator",
    "simulator",
    "gui",
    "graphics",
    "desktop",
    "display",
    "screen",
];

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Name of the service of runner `name`: the Windows service, the systemd unit (without
/// `.service`), the launchd label.
pub fn service_name(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("AlLocalRunner-{name}"),
        Os::Linux => format!("al-localrunner-runner-{name}"),
        Os::MacOs => format!("com.al-localrunner.runner-{name}"),
    }
}

/// The account the service runs under: the virtual service account on Windows, the installing
/// user elsewhere.
pub fn account(i: &RunnerServiceInputs) -> String {
    match i.os {
        Os::Windows => format!(r"NT SERVICE\{}", service_name(i.os, &i.name)),
        _ => i.user.clone(),
    }
}

/// The file that defines the service (Linux unit, macOS LaunchDaemon); `None` on Windows.
pub fn definition_path(os: Os, name: &str) -> Option<PathBuf> {
    let service = service_name(os, name);
    match os {
        Os::Windows => None,
        Os::Linux => Some(PathBuf::from(format!(
            "/etc/systemd/system/{service}.service"
        ))),
        Os::MacOs => Some(PathBuf::from(format!(
            "/Library/LaunchDaemons/{service}.plist"
        ))),
    }
}

fn is_root(user: &str) -> bool {
    let bare = user.rsplit('\\').next().unwrap_or(user);
    ["root", "system", "localsystem"].contains(&bare.to_ascii_lowercase().as_str())
}

/// Refuses root and `SYSTEM` as the runner's identity: a job would get the whole machine.
pub fn check_identity(i: &RunnerServiceInputs) -> Result<(), String> {
    identity_refusal(&i.user, None).map_or(Ok(()), Err)
}

/// Why `user` cannot be the runner's identity: root or `SYSTEM` by name, or by its id
/// `principal` (uid `0`, SID `S-1-5-18`), which catches an alias of root. `None` when it can.
pub fn identity_refusal(user: &str, principal: Option<&str>) -> Option<String> {
    let root_id = principal.is_some_and(|p| p == "0" || p.eq_ignore_ascii_case("S-1-5-18"));
    (is_root(user) || root_id).then(|| {
        format!(
            "a runner service never runs as {user}: run the command for the user the runner \
             belongs to (with sudo, or from an elevated terminal of that user)"
        )
    })
}

/// Who owns a file and whom its access list lets in: SIDs on Windows, the owner uid elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyAccess {
    /// The owner: a SID, or a uid.
    pub owner: Option<String>,
    /// The SIDs the access list allows; `None` when not known (always on Unix).
    pub allowed: Option<Vec<String>>,
}

/// What this process can tell of a file's ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyState {
    /// The file is not there.
    Missing,
    /// It is there, but its security cannot be read by this process (on Windows: a key that only
    /// a service account may touch).
    Unreadable,
    /// Its owner and access list.
    Read(KeyAccess),
}

const ADMINISTRATORS: &str = "S-1-5-32-544";

/// The ownership of `path` as this process sees it.
pub fn key_state(path: &Path) -> KeyState {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return KeyState::Missing,
        Err(_) => return KeyState::Unreadable,
        Ok(_) => {}
    }
    #[cfg(windows)]
    {
        match lr_net::file_security(path) {
            Ok(s) => KeyState::Read(KeyAccess {
                owner: s.owner,
                allowed: s.allowed,
            }),
            Err(_) => KeyState::Unreadable,
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(m) => KeyState::Read(KeyAccess {
                owner: Some(m.uid().to_string()),
                allowed: None,
            }),
            Err(_) => KeyState::Unreadable,
        }
    }
}

/// The id of OS user `user`: a SID on Windows, a uid elsewhere.
pub fn principal(user: &str) -> Option<String> {
    lr_net::account_principal(user).ok()
}

/// Whether a directory in `state` belongs to the user with id `user_id`: its owner is that user
/// (on Windows also the Administrators group, the owner of what an elevated shell creates).
pub fn dir_owned(os: Os, state: &KeyState, user_id: Option<&str>) -> bool {
    let (KeyState::Read(a), Some(user_id)) = (state, user_id) else {
        return false;
    };
    a.owner.as_deref().is_some_and(|o| {
        o.eq_ignore_ascii_case(user_id) || (os == Os::Windows && o == ADMINISTRATORS)
    })
}

fn held_by(a: &KeyAccess, sid: &str) -> bool {
    a.owner
        .as_deref()
        .is_some_and(|o| o.eq_ignore_ascii_case(sid))
        && a.allowed
            .as_ref()
            .is_some_and(|l| l.len() == 1 && l[0].eq_ignore_ascii_case(sid))
}

/// Why the Windows service of runner `name` may not take its key from `user` (id `user_id`):
/// the key is missing, belongs to someone else, or cannot be read. A key the user owns (or the
/// Administrators group owns with the user as its only entry) is the user's; a key that cannot
/// be read is accepted only when the installed service (its `--owner`, `service_owner`) already
/// belongs to `user`: a re-run. `None` when the key may be handed over.
pub fn key_handover_refusal(
    name: &str,
    user: &str,
    user_id: Option<&str>,
    key: &KeyState,
    service_owner: Option<Option<String>>,
) -> Option<String> {
    let user_id = user_id.unwrap_or("?");
    match key {
        KeyState::Missing => Some(format!(
            "runner {name} has no key of its own; join it to the broker first (`localrunner \
             runner join <address> --name {name}`)"
        )),
        KeyState::Read(a) => {
            let owner = a.owner.as_deref().unwrap_or("nobody");
            let only_user = a
                .allowed
                .as_ref()
                .is_some_and(|l| l.len() == 1 && l[0].eq_ignore_ascii_case(user_id));
            let mine =
                owner.eq_ignore_ascii_case(user_id) || (owner == ADMINISTRATORS && only_user);
            (!mine).then(|| {
                format!(
                    "the key of runner {name} belongs to {owner}, not to {user} ({user_id}): run \
                     the command for the runner's owner, from that user's elevated terminal"
                )
            })
        }
        KeyState::Unreadable => match service_owner {
            Some(Some(o)) if o.eq_ignore_ascii_case(user) => None,
            _ => Some(format!(
                "cannot read the key of runner {name} as {user}: it is not this user's; run the \
                 command for the runner's owner, from that user's elevated terminal"
            )),
        },
    }
}

/// The commands an administrator runs to give the key at `key` back to `user`, away from the
/// service `account`.
pub fn key_recovery(key: &Path, user: &str, account: &str) -> String {
    let k = key.display();
    format!(
        "takeown /F \"{k}\" /A; icacls \"{k}\" /inheritance:r /grant:r \"{user}:(F)\"; icacls \
         \"{k}\" /remove \"{account}\"; icacls \"{k}\" /setowner \"{user}\""
    )
}

/// To whom `autostart off` gives the key at `key` of runner `name` back on Windows: the user the
/// installed service names with `--owner` (`service_owner`: `None` when no service is
/// installed, `Some(None)` when it names nobody), never whoever runs `off`. `Ok(None)` when
/// there is nothing to give back (no key, no service); an error with the manual commands when
/// the user cannot be told.
pub fn restore_owner(
    name: &str,
    key: &Path,
    key_exists: bool,
    service_owner: Option<Option<String>>,
) -> Result<Option<String>, String> {
    match (key_exists, service_owner) {
        (false, _) | (_, None) => Ok(None),
        (true, Some(Some(user))) => Ok(Some(user)),
        (true, Some(None)) => Err(format!(
            "cannot tell whom runner {name} belongs to: its service {} names no --owner; give \
             the key back by hand as administrator, then run this command again: {}",
            service_name(Os::Windows, name),
            key_recovery(
                key,
                "<user>",
                &format!(r"NT SERVICE\{}", service_name(Os::Windows, name))
            )
        )),
    }
}

/// Checks the key given back to `user` (id `user_id`) by `autostart off`: owned by the user,
/// who is its only entry. `Ok(Some(note))` when this process cannot read it (another
/// administrator), with the command to check it as the user; an error with the manual commands
/// when the key is not the user's.
pub fn restore_verdict(
    name: &str,
    key: &Path,
    state: &KeyState,
    user: &str,
    user_id: Option<&str>,
    account: &str,
) -> Result<Option<String>, String> {
    match state {
        KeyState::Missing => Ok(None),
        KeyState::Read(a) if user_id.is_some_and(|id| held_by(a, id)) => Ok(None),
        KeyState::Read(a) => Err(format!(
            "the key of runner {name} was not given back to {user}: owner {}, entries {:?}; as \
             administrator run: {}",
            a.owner.as_deref().unwrap_or("none"),
            a.allowed.as_deref().unwrap_or_default(),
            key_recovery(key, user, account)
        )),
        KeyState::Unreadable => Ok(Some(format!(
            "this account cannot read the key of runner {name}; check as {user} that it is the \
             only entry: icacls \"{}\"",
            key.display()
        ))),
    }
}

/// The user the installed Windows service names with `--owner`, read from the output of
/// `sc qc` (`BINARY_PATH_NAME`); `None` when it names none.
pub fn owner_from_service_config(qc: &str) -> Option<String> {
    let line = qc
        .lines()
        .find(|l| l.trim_start().starts_with("BINARY_PATH_NAME"))?;
    let (_, command) = line.split_once(':')?;
    let after = command.split_once(" --owner ")?.1.trim_start();
    let owner = match after.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?.0,
        None => after.split_whitespace().next()?,
    };
    (!owner.is_empty()).then(|| owner.to_string())
}

/// The installed Windows service of runner `name` and the user it names with `--owner`:
/// `None` when there is no service, `Some(None)` when it names nobody.
pub fn service_owner(os: Os, name: &str) -> Option<Option<String>> {
    if os != Os::Windows {
        return installed(os, name).then_some(None);
    }
    let sc = super::programs::program_path("sc.exe").ok()?;
    let out = std::process::Command::new(sc)
        .args(["qc", &service_name(os, name), "4096"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| owner_from_service_config(&String::from_utf8_lossy(&out.stdout)))
}

/// The error of a failed Windows install with the command that cleans up after it.
pub fn install_failure(os: Os, name: &str, e: String) -> String {
    let admin = if os == Os::Windows {
        "from an elevated terminal"
    } else {
        "with sudo"
    };
    format!("{e}; to clean up, run {admin}: localrunner autostart off runner {name} --service")
}

/// Why a login autostart of runner `name` is refused: its OS service is `installed`, and both
/// would run it at once.
pub fn service_conflict(os: Os, name: &str, installed: bool) -> Option<String> {
    installed.then(|| {
        format!(
            "autostart runner {name}: it runs as the OS service {}, and a login autostart would \
             start it twice; remove the service first (`localrunner autostart off runner {name} \
             --service`, administrator) or keep it",
            service_name(os, name)
        )
    })
}

/// Why runner `name` gets no OS service: the `ci` runner, or a tag, platform or service among
/// `needs` that asks for a desktop session (an emulator, a simulator, graphics). A service runs
/// outside any user session, without a display. `None` when it may get one.
pub fn refusal(name: &str, needs: &[String]) -> Option<String> {
    if name == "ci" {
        return Some(
            "the `ci` runner gets no OS service: the GitHub Actions runner it drives needs the \
             user session (its toolchains and caches); keep its login autostart"
                .into(),
        );
    }
    let desktop = needs.iter().find(|n| {
        n.to_ascii_lowercase()
            .split(['-', '_', '.', ' '])
            .any(|w| DESKTOP_MARKERS.contains(&w))
    })?;
    Some(format!(
        "runner {name} declares `{desktop}`, which needs a desktop session; an OS service runs \
         without one: keep its login autostart"
    ))
}

/// Why runner `name` gets no service on `os` when it is not `joined` to a broker with its own
/// key: on Windows the service account is not the user, so it cannot use this user's local
/// broker channel (the broker admits it by OS user, and the channel names are the user's).
pub fn local_runner_refusal(os: Os, name: &str, joined: bool) -> Option<String> {
    (os == Os::Windows && !joined).then(|| {
        format!(
            "runner {name} reaches its broker over this user's local channel, which a Windows \
             service account cannot use; join it to the broker with its own key (`localrunner \
             broker add --runners 1`, then `localrunner runner join <address> --name {name}`) \
             or keep its login autostart"
        )
    })
}

/// Why runner `name` gets no service: its directory is missing (a typo, or the elevated run
/// looked at another installation) or not owned by `user`.
pub fn runner_dir_refusal(name: &str, exists: bool, owned: bool, user: &str) -> Option<String> {
    if !exists {
        return Some(format!(
            "there is no runner {name} in this installation; name an existing runner, and give \
             the user's installation with --state-dir when the command runs elevated"
        ));
    }
    (!owned).then(|| {
        format!(
            "the directory of runner {name} is not owned by {user}: this is another user's \
             installation; give the installing user's own with --state-dir"
        )
    })
}

/// Why runner `name` gets no service while a login entry of `user` also starts it: both would
/// run the same runner at once.
pub fn login_entry_refusal(name: &str, login: bool, user: &str) -> Option<String> {
    login.then(|| {
        format!(
            "runner {name} also starts at user login, and both would run it at once; remove that \
             first as {user}: localrunner autostart off runner {name}"
        )
    })
}

/// What `what` (`down`, `reload`, `upgrade`) says of runner `name` whose OS service it cannot
/// reach, given whether the service runs (`None`: unknown), with the command for an
/// administrator.
pub fn unreachable_line(os: Os, name: &str, running: Option<bool>, what: &str) -> String {
    use super::state::{named_restart_command, named_stop_command};
    let service = service_name(os, name);
    if running == Some(false) {
        return format!(
            "runner {name}: its OS service {service} is stopped; start it as administrator: {}",
            named_restart_command(os, &service)
        );
    }
    let (verb, command) = if what == "down" {
        ("stop", named_stop_command(os, &service))
    } else {
        ("restart", named_restart_command(os, &service))
    };
    format!(
        "runner {name} runs as the OS service {service}: {what} does not reach it; {verb} it as \
         administrator: {command}"
    )
}

/// What `upgrade` says of runner `name` installed as an OS service: the service keeps running
/// the old binary (Windows) or stays stopped after the drain (Linux, macOS), and `up` does not
/// start a copy of it, so an administrator restarts it.
pub fn upgrade_line(os: Os, name: &str) -> String {
    let service = service_name(os, name);
    format!(
        "runner {name} runs as the OS service {service}: upgrade replaces the binary but does \
         not restart the service; afterwards restart it as administrator: {}",
        super::state::named_restart_command(os, &service)
    )
}

/// The runner's key file, which the Windows service account must own.
pub fn key_path(i: &RunnerServiceInputs) -> PathBuf {
    i.state_dir
        .join("runner")
        .join(&i.name)
        .join(crate::build::KEY_FILE)
}

/// Whether a login entry of `user` starts runner `name` (a scheduler task, a systemd user unit,
/// a LaunchAgent); `false` when that cannot be told.
pub fn login_entry(os: Os, user: &str, name: &str) -> bool {
    let entry = super::autostart::Entry::Runner(Some(name.to_string())).name();
    match os {
        Os::Windows => {
            let Ok(principal) = lr_net::account_principal(user) else {
                return false;
            };
            let task = format!("Al.LocalRunner {entry} {principal}");
            super::programs::program_path("schtasks").is_ok_and(|p| {
                std::process::Command::new(p)
                    .args(["/Query", "/TN", &task])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
        }
        Os::Linux => home_of(user).is_some_and(|h| {
            h.join(".config/systemd/user")
                .join(format!("al-localrunner-{entry}.service"))
                .exists()
        }),
        Os::MacOs => home_of(user).is_some_and(|h| {
            h.join("Library/LaunchAgents")
                .join(format!("com.al-localrunner.{entry}.plist"))
                .exists()
        }),
    }
}

#[cfg(unix)]
fn home_of(user: &str) -> Option<PathBuf> {
    use std::ffi::{CStr, CString};
    let name = CString::new(user).ok()?;
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr((*entry).pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().into_owned()))
}

#[cfg(not(unix))]
fn home_of(_user: &str) -> Option<PathBuf> {
    None
}

/// The command line the service starts: `runner run --name <name> --service`, on Windows with
/// `--owner <user>` (the service account is not the installing user), and the directories.
pub fn run_args(i: &RunnerServiceInputs) -> Vec<String> {
    let mut args = vec![s(&i.exe)];
    args.extend(
        ["runner", "run", "--name", &i.name, "--service"]
            .iter()
            .map(|w| w.to_string()),
    );
    if i.os == Os::Windows {
        args.extend(["--owner".to_string(), i.user.clone()]);
    }
    args.extend(["--state-dir".to_string(), s(&i.state_dir)]);
    if let Some(c) = &i.config_dir {
        args.extend(["--config-dir".to_string(), s(c)]);
    }
    args
}

fn windows_dirs(i: &RunnerServiceInputs) -> Vec<(PathBuf, &'static str)> {
    let mut dirs = vec![(i.state_dir.clone(), "M")];
    if let Some(c) = &i.config_dir {
        dirs.push((c.clone(), "RX"));
    }
    if let Some(bin) = i.exe.parent() {
        dirs.push((bin.to_path_buf(), "RX"));
    }
    dirs
}

fn windows(i: &RunnerServiceInputs) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    let account = account(i);
    let bin_path = windows_command_line(&run_args(i));
    let display = format!("Al.LocalRunner runner {}", i.name);
    let params = |verb: &str| -> Vec<String> {
        [
            verb,
            &name,
            "binPath=",
            &bin_path,
            "start=",
            "demand",
            "obj=",
            &account,
            "DisplayName=",
            &display,
        ]
        .map(String::from)
        .to_vec()
    };
    let mut out = vec![
        Action::Run {
            program: "sc.exe".into(),
            args: params("create"),
            tolerate: Tolerate::Codes(vec![1073]),
        },
        Action::Run {
            program: "sc.exe".into(),
            args: params("config"),
            tolerate: Tolerate::Nothing,
        },
        Action::run("sc.exe", &["privs", &name, WINDOWS_PRIVILEGES]),
        Action::run(
            "sc.exe",
            &[
                "failure",
                &name,
                "reset=",
                "86400",
                "actions=",
                "restart/5000",
            ],
        ),
        Action::run("sc.exe", &["failureflag", &name, "1"]),
    ];
    for (dir, rights) in windows_dirs(i) {
        out.push(Action::run(
            "icacls",
            &[&s(&dir), "/grant", &format!("{account}:(OI)(CI){rights}")],
        ));
    }
    out.extend(key_to(i, &account, &i.user));
    out.push(Action::run("sc.exe", &["config", &name, "start=", "auto"]));
    out.push(Action::run("sc.exe", &["start", &name]).tolerating(Tolerate::Codes(vec![1056])));
    out
}

fn key_to(i: &RunnerServiceInputs, to: &str, from: &str) -> Vec<Action> {
    let key = s(&key_path(i));
    vec![
        Action::run("takeown", &["/F", &key, "/A"]),
        Action::run(
            "icacls",
            &[&key, "/inheritance:r", "/grant:r", &format!("{to}:(F)")],
        ),
        Action::run("icacls", &[&key, "/remove", from]),
        Action::run("icacls", &[&key, "/setowner", to]),
    ]
}

fn linux(i: &RunnerServiceInputs) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    let unit = format!(
        "[Unit]\nDescription=Al.LocalRunner runner {}\nAfter=network-online.target\n\
         Wants=network-online.target\n\n[Service]\nType=simple\nUser={}\nExecStart={}\n\
         Restart=on-failure\nRestartSec=5\nNoNewPrivileges=true\n\n[Install]\n\
         WantedBy=multi-user.target\n",
        i.name,
        i.user,
        systemd_command_line(&run_args(i))
    );
    vec![
        Action::WriteFile {
            path: definition_path(i.os, &i.name).unwrap_or_default(),
            contents: unit,
        },
        Action::run("systemctl", &["daemon-reload"]),
        Action::run(
            "systemctl",
            &["enable", "--now", &format!("{name}.service")],
        ),
    ]
}

fn macos(i: &RunnerServiceInputs) -> Vec<Action> {
    let label = service_name(i.os, &i.name);
    let path = definition_path(i.os, &i.name).unwrap_or_default();
    let extra = format!(
        "  <key>UserName</key>\n  <string>{}</string>\n  <key>KeepAlive</key>\n  <dict>\n    \
         <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n  <key>ThrottleInterval</key>\n  \
         <integer>5</integer>\n",
        xml(&i.user)
    );
    vec![
        Action::WriteFile {
            path: path.clone(),
            contents: plist(&label, &run_args(i), &extra),
        },
        Action::run("launchctl", &["bootout", &format!("system/{label}")])
            .tolerating(Tolerate::Anything),
        Action::run("launchctl", &["bootstrap", "system", &s(&path)]),
    ]
}

/// Whether runner `name` is registered as an OS service on this machine.
pub fn installed(os: Os, name: &str) -> bool {
    match definition_path(os, name) {
        Some(p) => p.exists(),
        None => super::programs::program_path("sc.exe").is_ok_and(|sc| {
            std::process::Command::new(sc)
                .args(["query", &service_name(os, name)])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }),
    }
}

/// Whether the OS service of runner `name` runs: `None` when that is not known (not installed,
/// the service manager cannot be asked).
pub fn running(os: Os, name: &str) -> Option<bool> {
    super::state::named_service_running(os, &service_name(os, name))
}

/// `autostart on runner <name> --service`: registers the runner with the service manager under
/// [`account`], started at boot and restarted after a failure, and starts it now. On Windows
/// the service account also gets the state directory (modify) and the configuration and binary
/// directories (read), and becomes the owner and only user of the runner's key ([`key_path`]),
/// as the key's owner check requires: the Administrators group takes the key first, so a re-run
/// over a service that already holds it works too. `uninstall` gives the key back to the user.
pub fn install(i: &RunnerServiceInputs) -> Vec<Action> {
    match i.os {
        Os::Windows => windows(i),
        Os::Linux => linux(i),
        Os::MacOs => macos(i),
    }
}

/// `autostart off runner <name> --service`: stops and removes that runner's service and nothing
/// else; its state stays. A service that is not there is fine. On Windows with `restore_key`
/// the key goes back to [`RunnerServiceInputs::user`] first, while the service account still
/// exists: the Administrators group takes it, grants it to the user alone and hands it over.
/// A failed step stops the plan before the service is deleted.
pub fn uninstall(i: &RunnerServiceInputs, restore_key: bool) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    match i.os {
        Os::Windows => {
            let account = account(i);
            let mut out =
                vec![Action::run("sc.exe", &["stop", &name]).tolerating(Tolerate::Anything)];
            if restore_key {
                out.extend(key_to(i, &i.user, &account));
            }
            for (dir, _) in windows_dirs(i) {
                out.push(
                    Action::run("icacls", &[&s(&dir), "/remove", &account])
                        .tolerating(Tolerate::Anything),
                );
            }
            out.push(
                Action::run("sc.exe", &["delete", &name]).tolerating(Tolerate::Codes(vec![1060])),
            );
            out
        }
        Os::Linux => vec![
            Action::run(
                "systemctl",
                &["disable", "--now", &format!("{name}.service")],
            )
            .tolerating(Tolerate::Anything),
            Action::RemoveFile(definition_path(i.os, &i.name).unwrap_or_default()),
            Action::run("systemctl", &["daemon-reload"]).tolerating(Tolerate::Anything),
        ],
        Os::MacOs => vec![
            Action::run("launchctl", &["bootout", &format!("system/{name}")])
                .tolerating(Tolerate::Anything),
            Action::RemoveFile(definition_path(i.os, &i.name).unwrap_or_default()),
        ],
    }
}
