//! A runner as an OS service (spec 3.12, 10, 10.3): it starts after a reboot without anyone
//! logging in and keeps running after logout. Windows: a service under its own virtual account
//! `NT SERVICE\AlLocalRunner-<name>`; Linux: a system unit under the installing user; macOS: a
//! LaunchDaemon under the installing user. Never under root or `SYSTEM`; installing and removing
//! it needs administrator rights.

use super::autostart::plist;
use super::quote::{systemd_command_line, windows_command_line, xml};
use super::{Action, Os, Tolerate};
use crate::runner_config::SecretFrom;
use std::collections::BTreeMap;
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
    /// The files of the `file:` sources of `[secret.*]`: the Windows service account gets read
    /// access to each, as to its own key.
    pub secret_files: Vec<PathBuf>,
    /// The files an earlier `autostart on` gave the Windows service account read access to
    /// ([`read_grants_record`]): taken back by `on` and `off` even when `runner.toml` no
    /// longer lists them.
    pub granted_files: Vec<PathBuf>,
}

/// Where `autostart on` records the secret files it gave read access to: outside the
/// directories of the service account, so a job cannot change the list.
pub fn grants_record(i: &RunnerServiceInputs) -> PathBuf {
    i.state_dir
        .join("service-grants")
        .join(format!("{}.json", i.name))
}

/// The files of the record at `path`; none when it is missing or unreadable.
pub fn read_grants_record(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .map(|files| files.into_iter().map(PathBuf::from).collect())
        .unwrap_or_default()
}

fn record_text(files: &[&PathBuf]) -> String {
    let files: Vec<String> = files.iter().map(|f| s(f)).collect();
    serde_json::to_string_pretty(&files).unwrap_or_else(|_| "[]".to_string())
}

fn stale_grants(i: &RunnerServiceInputs) -> Vec<&PathBuf> {
    i.granted_files
        .iter()
        .filter(|f| !i.secret_files.contains(f))
        .collect()
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
const SYSTEM: &str = "S-1-5-18";
const USERS: &str = "S-1-5-32-545";

/// The exit codes `icacls` gives for a path that is not there (`ERROR_FILE_NOT_FOUND`,
/// `ERROR_PATH_NOT_FOUND`): all `autostart off` tolerates when it takes the job root's right
/// back from a service installed before job roots existed.
pub const MISSING_PATH_CODES: [i32; 2] = [2, 3];

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
        .is_some_and(|o| o.eq_ignore_ascii_case(sid) || o == ADMINISTRATORS)
        && a.allowed
            .as_ref()
            .is_some_and(|l| l.len() == 1 && l[0].eq_ignore_ascii_case(sid))
}

/// Why the Windows service of runner `name` may not take its key at `path` (in state `key`)
/// from `user` (id `user_id`): the key is missing, belongs to someone else, or cannot be read.
/// A key the user owns, or the Administrators group owns with the user as its only entry, is
/// the user's. A key held by the service account (id `service_id`) is accepted only when the
/// installed service (its `--owner`, `service_owner`) belongs to `user`: a re-run; otherwise an
/// earlier run left it, and the refusal names the commands that give it back. `None` when the
/// key may be handed over.
pub fn key_handover_refusal(
    name: &str,
    user: &str,
    user_id: Option<&str>,
    service_id: Option<&str>,
    path: &Path,
    key: &KeyState,
    service_owner: Option<Option<String>>,
) -> Option<String> {
    let user_id = user_id.unwrap_or("?");
    let rerun = matches!(&service_owner, Some(Some(o)) if o.eq_ignore_ascii_case(user));
    let account = format!(r"NT SERVICE\{}", service_name(Os::Windows, name));
    match key {
        KeyState::Missing => Some(format!(
            "runner {name} has no key of its own; join it to the broker first (`localrunner \
             runner join <address> --name {name}`)"
        )),
        KeyState::Read(a) if held_by(a, user_id) || a.owner.as_deref() == Some(user_id) => None,
        KeyState::Read(a) if service_id.is_some_and(|id| held_by(a, id)) => (!rerun).then(|| {
            format!(
                "the key of runner {name} is still held by {account}, left by an earlier run; \
                 give it back to {user} as administrator, then run this command again:{}",
                key_recovery(path, user, &account)
            )
        }),
        KeyState::Read(a) => {
            let owner = a.owner.as_deref().unwrap_or("nobody");
            Some(format!(
                "the key of runner {name} belongs to {owner}, not to {user} ({user_id}): run the \
                 command for the runner's owner, from that user's elevated terminal"
            ))
        }
        KeyState::Unreadable => (!rerun).then(|| {
            format!(
                "cannot read the key of runner {name} as {user}: it is not this user's; run the \
                 command for the runner's owner, from that user's elevated terminal"
            )
        }),
    }
}

/// The commands an administrator runs to give the key at `key` back to `user`, away from the
/// service `account`, one per line (they work in cmd and in PowerShell alike). The
/// Administrators group stays the owner, which the key check accepts.
pub fn key_recovery(key: &Path, user: &str, account: &str) -> String {
    let k = key.display();
    format!(
        "\n  takeown /F \"{k}\" /A\n  icacls \"{k}\" /inheritance:r /grant:r \"{user}:(F)\"\n  \
         icacls \"{k}\" /remove \"{account}\""
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
             the key back by hand as administrator, then run this command again:{}",
            service_name(Os::Windows, name),
            key_recovery(
                key,
                "<user>",
                &format!(r"NT SERVICE\{}", service_name(Os::Windows, name))
            )
        )),
    }
}

/// Checks the key given back to `user` (id `user_id`) by `autostart off`: the user is its only
/// entry, and its owner is the user or the Administrators group. `Ok(Some(note))` when this
/// process cannot read it, with the command to check it as the user; an error with the manual
/// commands when the key is not the user's.
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
             administrator run:{}",
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

/// Checks the key handed to the service `account` (id `service_id`) by `autostart on`: the
/// account is its only entry, and its owner is the account or the Administrators group (who
/// can read it). An error with the commands that give it back to `user` otherwise.
pub fn handover_verdict(
    name: &str,
    key: &Path,
    state: &KeyState,
    account: &str,
    service_id: Option<&str>,
    user: &str,
) -> Result<(), String> {
    match state {
        KeyState::Read(a) if service_id.is_some_and(|id| held_by(a, id)) => Ok(()),
        _ => Err(format!(
            "the key of runner {name} was not handed to {account} alone ({state:?}); give it back \
             to {user} as administrator:{}\nthen run: localrunner autostart off runner {name} \
             --service",
            key_recovery(key, user, account)
        )),
    }
}

/// Splits a Windows command line the way programs read it (`CommandLineToArgvW`): the program
/// path ends at its closing quote or the first blank, then blanks separate arguments, quotes
/// group them, and backslashes escape only a quote.
pub fn split_command_line(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    if i == len {
        return Vec::new();
    }
    let mut first = String::new();
    if chars[i] == '"' {
        i += 1;
        while i < len && chars[i] != '"' {
            first.push(chars[i]);
            i += 1;
        }
        i += 1;
    } else {
        while i < len && !chars[i].is_whitespace() {
            first.push(chars[i]);
            i += 1;
        }
    }
    let mut out = vec![first];
    loop {
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= len {
            return out;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < len {
            let c = chars[i];
            if c == '\\' {
                let mut n = 0;
                while i < len && chars[i] == '\\' {
                    n += 1;
                    i += 1;
                }
                if i < len && chars[i] == '"' {
                    arg.push_str(&"\\".repeat(n / 2));
                    if n % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                } else {
                    arg.push_str(&"\\".repeat(n));
                }
                continue;
            }
            if c == '"' {
                if quoted && i + 1 < len && chars[i + 1] == '"' {
                    arg.push('"');
                    i += 2;
                    continue;
                }
                quoted = !quoted;
                i += 1;
                continue;
            }
            if c.is_whitespace() && !quoted {
                break;
            }
            arg.push(c);
            i += 1;
        }
        out.push(arg);
    }
}

fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().skip(1).position(|a| a == flag)? + 1;
    args.get(at + 1).map(String::as_str)
}

/// The user a service command line `runner run ... --owner <user>` names; `None` when it names
/// none. The line is split with Windows quoting, so ` --owner ` inside the quoted program path
/// is not taken for the flag.
pub fn owner_from_command_line(command: &str) -> Option<String> {
    value_of(&split_command_line(command), "--owner")
        .filter(|o| !o.is_empty())
        .map(str::to_string)
}

/// What the service manager holds for a Windows service: its command line and its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// The command line (`binPath`).
    pub command: String,
    /// The account it runs under (`obj`).
    pub account: String,
}

/// The configuration `install` registers for `i`.
pub fn service_config_for(i: &RunnerServiceInputs) -> ServiceConfig {
    ServiceConfig {
        command: windows_command_line(&run_args(i)),
        account: account(i),
    }
}

fn same_program(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        p.trim_matches('"')
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn same_dir(a: &str, b: &str) -> bool {
    let trim = |p: &str| p.trim_end_matches(['/', '\\']).to_lowercase();
    trim(a) == trim(b)
}

/// Why the installed service `config` of runner `name` is not one this installation registered
/// for `i`: another program (full path, ignoring case, quotes and `/` against `\`), not `runner run --name <name> --service`, another state directory,
/// or another account. Such a service is never changed or removed. `None` when it is ours.
pub fn not_ours(name: &str, config: &ServiceConfig, i: &RunnerServiceInputs) -> Option<String> {
    let args = split_command_line(&config.command);
    let expected = account(i);
    let exe = s(&i.exe);
    let reason = if !args.first().is_some_and(|p| same_program(p, &exe)) {
        format!(
            "it starts {}, while this command is {exe}; if the program was moved, run this \
             command with the program the service starts",
            args.first().map_or("nothing", String::as_str)
        )
    } else if args.get(1).map(String::as_str) != Some("runner")
        || args.get(2).map(String::as_str) != Some("run")
        || value_of(&args, "--name") != Some(name)
        || !args.iter().any(|a| a == "--service")
    {
        format!(
            "it does not run runner {name} as a service: {}",
            config.command
        )
    } else if !value_of(&args, "--state-dir").is_some_and(|d| same_dir(d, &s(&i.state_dir))) {
        format!(
            "it belongs to the installation in {}",
            value_of(&args, "--state-dir").unwrap_or("an unknown directory")
        )
    } else if !config.account.eq_ignore_ascii_case(&expected) {
        format!("it runs as {}, not as {expected}", config.account)
    } else {
        return None;
    };
    Some(format!(
        "the service {} is not one this installation registered ({reason}); it is left alone: \
         remove it by hand, or give the installation it belongs to with --state-dir",
        service_name(Os::Windows, name)
    ))
}

/// The installed Windows service of runner `name` as the service manager holds it: `None` when
/// there is no service, `Some(None)` when its configuration cannot be read. On Linux and macOS
/// `Some(None)` for an installed service: its definition is not read back.
pub fn service_config(os: Os, name: &str) -> Option<Option<ServiceConfig>> {
    if os != Os::Windows {
        return installed(os, name).then_some(None);
    }
    #[cfg(windows)]
    {
        read_service_config(&service_name(os, name))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn read_service_config(service: &str) -> Option<Option<ServiceConfig>> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_SERVICE_DOES_NOT_EXIST};
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW,
        QUERY_SERVICE_CONFIGW, SC_MANAGER_CONNECT, SERVICE_QUERY_CONFIG,
    };
    unsafe fn text(p: *const u16) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)))
    }
    let name: Vec<u16> = service.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Some(None);
        }
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_CONFIG);
        if svc.is_null() {
            let missing = GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST;
            CloseServiceHandle(scm);
            return (!missing).then_some(None);
        }
        let mut needed = 0u32;
        QueryServiceConfigW(svc, std::ptr::null_mut(), 0, &mut needed);
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        let config = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
        let size = (buf.len() * 8) as u32;
        let read = if QueryServiceConfigW(svc, config, size, &mut needed) != 0 {
            text((*config).lpBinaryPathName).map(|command| ServiceConfig {
                command,
                account: text((*config).lpServiceStartName).unwrap_or_default(),
            })
        } else {
            None
        };
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        Some(read)
    }
}

/// Asks `probe` for the state of a service up to `polls` times, calling `pause` between asks,
/// until it reports `want` `settle` times in a row. `Err` with the last answer otherwise.
pub fn confirm_state(
    probe: &mut dyn FnMut() -> Option<bool>,
    pause: &mut dyn FnMut(),
    polls: usize,
    settle: usize,
    want: bool,
) -> Result<(), Option<bool>> {
    let (mut streak, mut last) = (0, None);
    for n in 0..polls {
        last = probe();
        if last == Some(want) {
            streak += 1;
            if streak >= settle {
                return Ok(());
            }
        } else {
            streak = 0;
        }
        if n + 1 < polls {
            pause();
        }
    }
    Err(last)
}

/// What `autostart on` says when the service of runner `name` started but did not keep running
/// (`state`: its last state, `None` unknown), with the runner's log `log` and the state command.
pub fn start_failure(os: Os, name: &str, state: Option<bool>, log: &Path) -> String {
    let service = service_name(os, name);
    let state = match state {
        Some(true) => "running, but not steadily",
        Some(false) => "not running",
        None => "unknown",
    };
    format!(
        "runner {name}: the OS service {service} was started but did not keep running (state: \
         {state}); the reason is in {}; its state: {}",
        log.display(),
        super::state::named_status_command(os, &service)
    )
}

/// Applies the install plan of `i` through `system`. A service that `existed` before this run,
/// or that `sc create` finds already there (code 1073, created by someone else meanwhile), is
/// never undone: the message says whether its definition was changed (the single `sc config`
/// comes after the risky steps). After a 1073 the service is read again (`reread`) and nothing
/// more is done unless it is this installation's ([`not_ours`]). A service this run created on
/// Windows is undone once `stopped` confirms it stopped (the key goes back to the user when a
/// key step had run, then the service is removed); the undo stops at a service that keeps
/// running or at a failed key step, keeping the service and printing the commands to finish.
pub fn apply_install(
    system: &dyn super::System,
    i: &RunnerServiceInputs,
    existed: bool,
    reread: &mut dyn FnMut() -> Option<Option<ServiceConfig>>,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<(), String> {
    let plan = install(i);
    let mut existed = existed;
    let mut failed = None;
    for (n, a) in plan.iter().enumerate() {
        let created = matches!(a, Action::Run { program, args, .. }
            if program == "sc.exe" && args.first().is_some_and(|w| w == "create"));
        if created {
            let strict = match a {
                Action::Run { program, args, .. } => Action::Run {
                    program: program.clone(),
                    args: args.clone(),
                    tolerate: Tolerate::Nothing,
                },
                other => other.clone(),
            };
            match system.apply(&strict) {
                Ok(()) => continue,
                Err(e) if e.starts_with("exit code 1073") => {
                    let why = match reread() {
                        Some(Some(config)) => not_ours(&i.name, &config, i),
                        Some(None) => Some(format!(
                            "its configuration cannot be read; it is left alone; check it as \
                             administrator: {}",
                            super::state::named_status_command(i.os, &service_name(i.os, &i.name))
                        )),
                        None => Some(
                            "it was gone again when read; it is left alone; run the command again"
                                .to_string(),
                        ),
                    };
                    if let Some(why) = why {
                        return Err(format!(
                            "{a}: {e}; the service {} appeared after this run checked for it, so \
                             nothing was changed: {why}",
                            service_name(i.os, &i.name)
                        ));
                    }
                    existed = true;
                    continue;
                }
                Err(e) => {
                    failed = Some((n, format!("{a}: {e}")));
                    break;
                }
            }
        }
        if let Err(e) = system.apply(a) {
            failed = Some((n, format!("{a}: {e}")));
            break;
        }
    }
    let Some((at, e)) = failed else {
        return Ok(());
    };
    let service = service_name(i.os, &i.name);
    if existed {
        let config = plan.iter().position(|a| {
            matches!(a, Action::Run { program, args, .. }
                if program == "sc.exe" && args.first().is_some_and(|w| w == "config"))
        });
        let what = match config {
            Some(c) if at < c => {
                "its definition was not changed (its privileges and recovery are set again to \
                 the same values; its directory and key rights stay with its account)"
            }
            _ => "its definition was updated to this run's, but it did not start",
        };
        return Err(format!(
            "{e}; the service {service} existed before this run and was not removed; {what}; fix \
             the cause and run the command again; its state: {}",
            super::state::named_status_command(i.os, &service)
        ));
    }
    if i.os != Os::Windows {
        return Err(install_failure(i.os, &i.name, e));
    }
    let key = s(&key_path(i));
    let touched = plan[..=at]
        .iter()
        .any(|a| matches!(a, Action::Run { args, .. } if args.contains(&key)));
    match apply_uninstall(system, i, touched, stopped) {
        Ok(()) => Err(format!(
            "{e} (undone: the service this run registered was removed{})",
            if touched {
                format!(" and the key given back to {}", i.user)
            } else {
                String::new()
            }
        )),
        Err(u) => Err(format!(
            "{e}; undoing it: {u}\nthen run: localrunner autostart off runner {} --service",
            i.name
        )),
    }
}

/// Applies the uninstall plan of `i` through `system`. On Windows, after `sc stop` it asks
/// `stopped` whether the service stopped and changes nothing else while it runs; a failed key
/// step stops before `sc delete`, with the commands that give the key back; rights on the
/// directories that could not be taken back are named with their commands after the service is
/// removed, and make the result an error.
pub fn apply_uninstall(
    system: &dyn super::System,
    i: &RunnerServiceInputs,
    restore_key: bool,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<(), String> {
    let plan = uninstall(i, restore_key);
    if i.os != Os::Windows {
        return super::apply_all(system, &plan);
    }
    let service = service_name(i.os, &i.name);
    let account = account(i);
    let Some((stop, rest)) = plan.split_first() else {
        return Ok(());
    };
    system.apply(stop).map_err(|e| format!("{stop}: {e}"))?;
    if !stopped() {
        return Err(format!(
            "the service {service} did not stop, so nothing else was changed; stop it as \
             administrator: {}, then run this command again",
            super::state::named_stop_command(i.os, &service)
        ));
    }
    let dirs: Vec<String> = windows_grants(i).iter().map(|g| s(&g.path)).collect();
    let key = s(&key_path(i));
    let mut missed = Vec::new();
    for a in rest {
        let Err(e) = system.apply(a) else {
            continue;
        };
        if let Action::Run { args, .. } = a {
            if let Some(dir) = args.first().filter(|d| dirs.contains(d)) {
                missed.push(format!("  icacls \"{dir}\" /remove \"{account}\""));
                continue;
            }
            if args.contains(&key) {
                return Err(format!(
                    "{a}: {e}; the key of runner {} may not be {}'s again, and the service \
                     {service} was kept; as administrator run:{}",
                    i.name,
                    i.user,
                    key_recovery(&key_path(i), &i.user, &account)
                ));
            }
        }
        let mut e = format!("{a}: {e}");
        if !missed.is_empty() {
            e.push_str(&format!(
                "; the rights of {account} were not taken back from every directory either; as \
                 administrator run:\n{}",
                missed.join("\n")
            ));
        }
        return Err(e);
    }
    if missed.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the service {service} was removed, but the rights of {account} were not taken back from \
         every directory; as administrator run:\n{}",
        missed.join("\n")
    ))
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
             service account cannot use; {}, or keep its login autostart",
            join_steps(name)
        )
    })
}

/// The commands that give a local runner its own broker key, after which it may become a
/// service: `broker add`, `runner join`, then `autostart on runner <name> --service`.
pub fn join_steps(name: &str) -> String {
    format!(
        "join it to the broker with its own key (`localrunner broker add --runners 1`, then \
         `localrunner runner join <address> --name {name}`, then `localrunner autostart on \
         runner {name} --service`)"
    )
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

/// Where the jobs of the Windows service of `i.name` work: `<name>` in
/// `%ProgramData%\Al.LocalRunner.Jobs`, whose parents every account can read and only the service
/// account, the Administrators group, `SYSTEM` and the installing user may write. A program of
/// a job that walks its path upwards (Git does) cannot do that through the state directory,
/// which is closed to everyone else. `None` elsewhere: the jobs work in the runner's own state
/// directory.
pub fn job_root(i: &RunnerServiceInputs) -> Option<PathBuf> {
    (i.os == Os::Windows).then(|| crate::paths::jobs_parent().join(&i.name))
}

/// Why `runner run --job-root` is refused: it is for the Windows service (`--service`) only.
/// `None` when it may go ahead.
pub fn job_root_refusal(given: bool, service: bool, windows: bool) -> Option<String> {
    if !given {
        return None;
    }
    if !windows {
        return Some(
            "--job-root is for the Windows runner service only: on this OS jobs work in the \
             runner's state directory"
                .into(),
        );
    }
    (!service).then(|| {
        "--job-root needs --service: it is set by `autostart on runner <name> --service`, and a \
         runner started by hand works in its state directory"
            .into()
    })
}

/// The job roots left under `parent` (one directory per runner); empty when there are none.
pub fn leftover_job_roots(parent: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

/// The command line the service starts: `runner run --name <name> --service`, on Windows with
/// `--owner <user>` (the service account is not the installing user) and `--job-root`, and the
/// directories.
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
    if let Some(root) = job_root(i) {
        args.extend(["--job-root".to_string(), s(&root)]);
    }
    args.extend(["--state-dir".to_string(), s(&i.state_dir)]);
    if let Some(c) = &i.config_dir {
        args.extend(["--config-dir".to_string(), s(c)]);
    }
    args
}

/// What a job of the Windows service of `i.name` can reach beyond this machine: its virtual
/// account authenticates on the network as the computer account (`DOMAIN\HOST$` on a machine
/// joined to a domain), so it gets what the network grants to this computer.
pub fn network_identity_note(i: &RunnerServiceInputs) -> String {
    format!(
        "{} reaches the network as this computer's account (DOMAIN\\HOST$ on a machine joined to \
         a domain): a job of runner {} can use what network shares and services grant to this \
         computer; keep the login autostart instead where that is not acceptable",
        account(i),
        i.name
    )
}

/// What a runner of `needs` (its tags, platforms and services) and `secrets` (its
/// `[secret.*]` sources) should know before it becomes the OS service of `i`, one line each:
/// the service has no `PATH` of the user and (on Windows) reaches the network as the computer
/// account, stopping it ends running jobs without a drain, an `env:` secret is invisible to
/// it, and a `docker` tag asks for the Docker engine. Names and variable names only, never a
/// value.
pub fn service_notes(
    i: &RunnerServiceInputs,
    needs: &[String],
    secrets: &BTreeMap<String, SecretFrom>,
) -> Vec<String> {
    let mut out = Vec::new();
    if i.os == Os::Windows {
        out.push(
            "the service account does not have your PATH or the tools installed for your user \
             only: jobs see only tools installed for all users"
                .to_string(),
        );
        out.push(network_identity_note(i));
    }
    out.push(format!(
        "stopping the service ends running jobs at once, without a drain; use `localrunner down \
         {}` first where it reaches the runner",
        i.name
    ));
    for (secret, from) in secrets {
        match from {
            SecretFrom::Env(var) => out.push(format!(
                "[secret.{secret}] from = \"env:{var}\": the service does not see the \
                 variables of your shell or user ({}); the secret will be missing for its jobs; \
                 use from = \"file:PATH\" with a file only the service account reads",
                service_env_hint(i.os)
            )),
            SecretFrom::File(path) => out.extend(file_secret_notes(i, secret, path)),
            SecretFrom::Keychain(_) => {}
        }
    }
    if needs.iter().any(|n| {
        n.to_ascii_lowercase()
            .split(['-', '_', '.', ' '])
            .any(|w| w == "docker")
    }) {
        out.push(format!(
            "runner {} declares `docker`: {}",
            i.name,
            docker_hint(i)
        ));
    }
    out
}

fn file_secret_notes(i: &RunnerServiceInputs, secret: &str, path: &Path) -> Vec<String> {
    let shown = path.display();
    if !path.is_file() {
        return vec![format!(
            "[secret.{secret}] from = \"file:{shown}\": the file does not exist; create it \
             before a job asks for the secret, which otherwise ends with secret_missing"
        )];
    }
    let mut out = Vec::new();
    if i.os == Os::Windows {
        out.push(format!(
            "[secret.{secret}] from = \"file:{shown}\": the service account gets read access to \
             the file, and `autostart off` takes it back; remove the access of other users \
             yourself, the file holds the secret"
        ));
    }
    if group_or_other_can_use(path) {
        out.push(format!(
            "[secret.{secret}] from = \"file:{shown}\": other users can read the file; \
             restrict it to {} (chmod 600)",
            i.user
        ));
    }
    out
}

#[cfg(unix)]
fn group_or_other_can_use(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

#[cfg(not(unix))]
fn group_or_other_can_use(_path: &Path) -> bool {
    false
}

fn service_env_hint(os: Os) -> &'static str {
    match os {
        Os::Windows => "only machine-level variables reach a Windows service",
        Os::Linux => "only what the systemd unit sets reaches it",
        Os::MacOs => "only what the LaunchDaemon plist sets reaches it",
    }
}

fn docker_hint(i: &RunnerServiceInputs) -> String {
    match i.os {
        Os::Windows => "the service account is not you and cannot open the pipe of Docker \
            Desktop, which is the signed-in user's: its jobs may not reach the Docker engine"
            .to_string(),
        Os::Linux => format!(
            "`{}` must be in the `docker` group when the service starts, or its jobs cannot \
             reach the Docker engine",
            i.user
        ),
        Os::MacOs => "a LaunchDaemon does not see the Docker Desktop socket of your home, so \
            its jobs may not reach the Docker engine"
            .to_string(),
    }
}

/// A right the Windows service account gets on one path for as long as the service exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The directory or file.
    pub path: PathBuf,
    /// The `icacls` rights, with the inheritance flags of a directory.
    pub rights: &'static str,
    /// Whether the path may be missing (a configuration file the user has not written): a
    /// missing one is skipped when the right is given and taken back.
    pub optional: bool,
}

/// What the Windows service account of runner `i.name` may touch, and nothing more of the state
/// directory: its own directory `runner/<name>` (modify: database, logs, key), its job root
/// ([`job_root`], modify: workspace, caches, temporary files), the
/// shared disk reservations (modify), `machine-id` (read), the configuration it reads
/// (`runner.toml` and `logging.toml`; the whole configuration directory when it is a separate
/// one) and the binary's directory (read). The broker's and the hub's databases, keys, policy
/// and logs and the other runners' directories stay out of its reach.
pub fn windows_grants(i: &RunnerServiceInputs) -> Vec<Grant> {
    let grant = |path: PathBuf, rights: &'static str, optional: bool| Grant {
        path,
        rights,
        optional,
    };
    let mut out = vec![
        grant(i.state_dir.join("runner").join(&i.name), "(OI)(CI)M", false),
        grant(
            i.state_dir.join(crate::build::DISK_LEDGER),
            "(OI)(CI)M",
            false,
        ),
        grant(i.state_dir.join(crate::host::MACHINE_ID_FILE), "(R)", false),
    ];
    if let Some(root) = job_root(i) {
        out.insert(1, grant(root, "(OI)(CI)M", false));
    }
    for file in &i.secret_files {
        out.push(grant(file.clone(), "(R)", true));
    }
    match &i.config_dir {
        Some(c) => out.push(grant(c.clone(), "(OI)(CI)RX", false)),
        None => {
            for file in ["runner.toml", "logging.toml"] {
                out.push(grant(i.state_dir.join(file), "(R)", true));
            }
        }
    }
    if let Some(bin) = i.exe.parent() {
        out.push(grant(bin.to_path_buf(), "(OI)(CI)RX", false));
    }
    out
}

fn tolerated(optional: bool) -> Tolerate {
    match optional {
        true => Tolerate::Anything,
        false => Tolerate::Nothing,
    }
}

fn windows(i: &RunnerServiceInputs) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    let account = account(i);
    let bin_path = windows_command_line(&run_args(i));
    let display = format!("Al.LocalRunner runner {}", i.name);
    let params = |verb: &str, start: &str| -> Vec<String> {
        [
            verb,
            &name,
            "binPath=",
            &bin_path,
            "start=",
            start,
            "obj=",
            &account,
            "DisplayName=",
            &display,
        ]
        .map(String::from)
        .to_vec()
    };
    let mut out = vec![Action::Run {
        program: "sc.exe".into(),
        args: params("create", "demand"),
        tolerate: Tolerate::Codes(vec![1073]),
    }];
    out.push(Action::CreateDir(
        i.state_dir.join(crate::build::DISK_LEDGER),
    ));
    if let Some(root) = job_root(i) {
        let parent = crate::paths::jobs_parent();
        out.push(Action::CreateDir(parent.clone()));
        out.push(Action::RefuseLink(parent.clone()));
        out.push(Action::run(
            "icacls",
            &[&s(&parent), "/setowner", &format!("*{ADMINISTRATORS}")],
        ));
        out.push(Action::run(
            "icacls",
            &[
                &s(&parent),
                "/inheritance:r",
                "/grant:r",
                &format!("*{ADMINISTRATORS}:(OI)(CI)F"),
                &format!("*{SYSTEM}:(OI)(CI)F"),
                &format!("*{USERS}:(RX)"),
            ],
        ));
        out.push(Action::CreateDir(root.clone()));
        out.push(Action::RefuseLink(root.clone()));
        out.push(Action::run(
            "icacls",
            &[&s(&root), "/setowner", &format!("*{ADMINISTRATORS}")],
        ));
        out.push(Action::run(
            "icacls",
            &[
                &s(&root),
                "/inheritance:r",
                "/grant:r",
                &format!("*{ADMINISTRATORS}:(OI)(CI)F"),
                &format!("*{SYSTEM}:(OI)(CI)F"),
                &format!("{}:(OI)(CI)F", i.user),
            ],
        ));
    }
    let stale = stale_grants(i);
    if !i.secret_files.is_empty() || !i.granted_files.is_empty() {
        let mut both: Vec<&PathBuf> = i.granted_files.iter().collect();
        both.extend(
            i.secret_files
                .iter()
                .filter(|f| !i.granted_files.contains(f)),
        );
        out.push(Action::WriteFile {
            path: grants_record(i),
            contents: record_text(&both),
        });
    }
    for g in windows_grants(i) {
        out.push(
            Action::run(
                "icacls",
                &[&s(&g.path), "/grant", &format!("{account}:{}", g.rights)],
            )
            .tolerating(tolerated(g.optional)),
        );
    }
    if !stale.is_empty() {
        for file in &stale {
            out.push(
                Action::run("icacls", &[&s(file), "/remove", &account])
                    .tolerating(Tolerate::Anything),
            );
        }
        out.push(Action::WriteFile {
            path: grants_record(i),
            contents: record_text(&i.secret_files.iter().collect::<Vec<_>>()),
        });
    }
    out.extend(key_to(i, &account, &i.user));
    out.extend([
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
        Action::Run {
            program: "sc.exe".into(),
            args: params("config", "auto"),
            tolerate: Tolerate::Nothing,
        },
    ]);
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
    ]
}

fn linux(i: &RunnerServiceInputs) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    let unit = format!(
        "[Unit]\nDescription=Al.LocalRunner runner {}\nAfter=network-online.target\n\
         Wants=network-online.target\n\n[Service]\nType=simple\nUser={}\nExecStart={}\n\
         Restart=on-failure\nRestartSec=5\nNoNewPrivileges=true\nDelegate=yes\n\n\
         [Install]\nWantedBy=multi-user.target\n",
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
/// [`account`], started at boot and restarted after a failure, and starts it now. On Linux the
/// unit delegates its cgroup subtree (`Delegate=yes`): a system service has no user manager,
/// so the runner makes the job cgroups of the `limit` mode itself. On Windows
/// the service account also gets the rights of [`windows_grants`] (its own runner directory and
/// its job root, never the whole state directory; the job root is created closed to everyone
/// but the service account, the Administrators group, `SYSTEM` and the installing user), and becomes the only user of the runner's key
/// ([`key_path`]): the
/// Administrators group takes the key (so a re-run over a service that already holds it works
/// too) and stays its owner, which the key's owner check accepts. `uninstall` gives the key
/// back to the user.
pub fn install(i: &RunnerServiceInputs) -> Vec<Action> {
    match i.os {
        Os::Windows => windows(i),
        Os::Linux => linux(i),
        Os::MacOs => macos(i),
    }
}

/// `autostart off runner <name> --service`: stops and removes that runner's service and nothing
/// else; its state and its job root stay. A service that is not there is fine. On Windows with `restore_key`
/// the key goes back to [`RunnerServiceInputs::user`] first, while the service account still
/// exists: the Administrators group takes it and grants it to the user alone.
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
            let jobs = job_root(i);
            for g in windows_grants(i) {
                let tolerate = if jobs.as_ref() == Some(&g.path) {
                    Tolerate::Codes(MISSING_PATH_CODES.to_vec())
                } else {
                    tolerated(g.optional)
                };
                out.push(
                    Action::run("icacls", &[&s(&g.path), "/remove", &account]).tolerating(tolerate),
                );
            }
            for file in stale_grants(i) {
                out.push(
                    Action::run("icacls", &[&s(file), "/remove", &account])
                        .tolerating(Tolerate::Anything),
                );
            }
            out.push(
                Action::run("sc.exe", &["delete", &name]).tolerating(Tolerate::Codes(vec![1060])),
            );
            out.push(Action::RemoveFile(grants_record(i)));
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
