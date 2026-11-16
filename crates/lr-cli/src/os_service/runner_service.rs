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
    if is_root(&i.user) {
        return Err(format!(
            "a runner service never runs as {}: run the command for the user the runner belongs \
             to (with sudo, or from an elevated terminal of that user)",
            i.user
        ));
    }
    Ok(())
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
    out.push(Action::run("sc.exe", &["config", &name, "start=", "auto"]));
    out.push(Action::run("sc.exe", &["start", &name]).tolerating(Tolerate::Codes(vec![1056])));
    out
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
/// directories (read).
pub fn install(i: &RunnerServiceInputs) -> Vec<Action> {
    match i.os {
        Os::Windows => windows(i),
        Os::Linux => linux(i),
        Os::MacOs => macos(i),
    }
}

/// `autostart off runner <name> --service`: stops and removes that runner's service and nothing
/// else; its state stays. A service that is not there is fine.
pub fn uninstall(i: &RunnerServiceInputs) -> Vec<Action> {
    let name = service_name(i.os, &i.name);
    match i.os {
        Os::Windows => {
            let account = account(i);
            let mut out =
                vec![Action::run("sc.exe", &["stop", &name]).tolerating(Tolerate::Anything)];
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
