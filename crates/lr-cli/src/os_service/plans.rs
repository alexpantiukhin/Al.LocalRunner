//! The plans: pure functions from inputs to [`Action`]s, one per OS, so each can be checked on
//! any machine without touching it.

pub use super::autostart::{autostart_off, autostart_on, task_name, AutostartInputs, Entry};

use super::autostart::plist;
use super::quote::{systemd_command_line, windows_command_line, xml};
use super::{service_account, service_group, service_name, Action, Os, Tolerate};
use std::path::{Path, PathBuf};

/// What `broker install-service` needs.
#[derive(Debug, Clone)]
pub struct ServiceInputs {
    /// Target OS.
    pub os: Os,
    /// The binary to install for the service (normally this one).
    pub exe: PathBuf,
    /// Where the service binary goes ([`super::service_binary`]).
    pub binary: PathBuf,
    /// The service state directory ([`crate::paths::service_root`]).
    pub root: PathBuf,
    /// Configuration directory with the files `init` created (`policy.toml`, `logging.toml`).
    pub configs: PathBuf,
    /// The OS user who installs: listed in `[[local_user]]`, a member of the socket group.
    pub user: String,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn service_args(i: &ServiceInputs) -> Vec<String> {
    vec![
        s(&i.binary),
        "broker".into(),
        "run".into(),
        "--service".into(),
        "--state-dir".into(),
        s(&i.root),
    ]
}

fn secure(i: &ServiceInputs) -> Action {
    Action::SecureServiceDir {
        path: i.root.clone(),
        account: service_account(i.os).into(),
    }
}

fn configs(i: &ServiceInputs) -> Vec<Action> {
    let mut out: Vec<Action> = ["policy.toml", "logging.toml"]
        .iter()
        .map(|f| Action::CopyIfMissing {
            from: i.configs.join(f),
            to: i.root.join(f),
        })
        .collect();
    out.push(Action::EnsureLocalUser {
        policy: i.root.join("policy.toml"),
        user: i.user.clone(),
    });
    out
}

fn copy_binary(i: &ServiceInputs) -> Action {
    Action::Copy {
        from: i.exe.clone(),
        to: i.binary.clone(),
    }
}

fn windows(i: &ServiceInputs) -> Vec<Action> {
    let name = service_name(i.os);
    let account = service_account(i.os);
    let bin_path = windows_command_line(&service_args(i));
    let params = |verb: &str| -> Vec<String> {
        [
            verb,
            name,
            "binPath=",
            &bin_path,
            "start=",
            "demand",
            "obj=",
            account,
            "DisplayName=",
            "Al.LocalRunner broker",
        ]
        .map(String::from)
        .to_vec()
    };
    let mut out = vec![
        Action::run("net", &["stop", name]).tolerating(Tolerate::Anything),
        copy_binary(i),
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
        Action::run(
            "sc.exe",
            &[
                "failure",
                name,
                "reset=",
                "86400",
                "actions=",
                "restart/5000",
            ],
        ),
        Action::run("sc.exe", &["failureflag", name, "1"]),
        secure(i),
    ];
    out.extend(configs(i));
    out.push(secure(i));
    out.push(Action::run("sc.exe", &["config", name, "start=", "auto"]));
    out.push(Action::run("sc.exe", &["start", name]).tolerating(Tolerate::Codes(vec![1056])));
    out
}

/// What undoes a failed `broker install-service`: on Windows the service is registered before
/// its directory is checked (its account must exist), so a service this run created is
/// deleted again; one that `existed` before stays, left on manual start by the plan. Other
/// systems register the service only as the last steps.
pub fn install_service_undo(os: Os, existed: bool) -> Vec<Action> {
    if os != Os::Windows || existed {
        return Vec::new();
    }
    let name = service_name(os);
    vec![
        Action::run("sc.exe", &["stop", name]).tolerating(Tolerate::Anything),
        Action::run("sc.exe", &["delete", name]).tolerating(Tolerate::Anything),
    ]
}

fn chown_and_close(i: &ServiceInputs, dir_mode: &str) -> Vec<Action> {
    let account = service_account(i.os);
    let group = service_group(i.os);
    let root = s(&i.root);
    vec![
        Action::run("chown", &["-R", &format!("{account}:{group}"), &root]),
        Action::run("chmod", &["-R", "go-rwx", &root]),
        Action::run("chmod", &[dir_mode, &root]),
    ]
}

fn linux(i: &ServiceInputs) -> Vec<Action> {
    let name = service_name(i.os);
    let account = service_account(i.os);
    let group = service_group(i.os);
    let root = s(&i.root);
    let mut out = vec![
        Action::run("groupadd", &["--system", group]).tolerating(Tolerate::Codes(vec![9])),
        Action::run(
            "useradd",
            &[
                "--system",
                "--gid",
                group,
                "--home-dir",
                &root,
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                account,
            ],
        )
        .tolerating(Tolerate::Codes(vec![9])),
        Action::run("usermod", &["-aG", group, &i.user]),
        copy_binary(i),
        secure(i),
    ];
    out.extend(configs(i));
    out.extend(chown_and_close(i, "0700"));
    let unit = format!(
        "[Unit]\nDescription=Al.LocalRunner pool broker\nAfter=network-online.target\n\
         Wants=network-online.target\n\n[Service]\nType=simple\nUser={account}\nGroup={group}\n\
         ExecStart={}\nRuntimeDirectory=al-localrunner\nRuntimeDirectoryMode=0750\nUMask=0077\n\
         Restart=on-failure\nRestartSec=5\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n",
        systemd_command_line(&service_args(i))
    );
    out.push(Action::WriteFile {
        path: PathBuf::from(format!("/etc/systemd/system/{name}.service")),
        contents: unit,
    });
    out.push(Action::run("systemctl", &["daemon-reload"]));
    out.push(Action::run(
        "systemctl",
        &["enable", "--now", &format!("{name}.service")],
    ));
    out
}

fn macos(i: &ServiceInputs) -> Vec<Action> {
    let name = service_name(i.os);
    let account = service_account(i.os);
    let group = service_group(i.os);
    let ipc = i.root.join("ipc");
    let mut out = vec![
        Action::EnsureMacAccount {
            name: account.into(),
            home: i.root.clone(),
        },
        Action::run(
            "dseditgroup",
            &["-o", "edit", "-a", &i.user, "-t", "user", group],
        ),
        copy_binary(i),
        secure(i),
    ];
    out.extend(configs(i));
    out.push(Action::CreateDir(ipc.clone()));
    out.extend(chown_and_close(i, "0710"));
    out.push(Action::run("chmod", &["0750", &s(&ipc)]));
    let extra = format!(
        "  <key>UserName</key>\n  <string>{}</string>\n  <key>GroupName</key>\n  \
         <string>{}</string>\n  <key>Umask</key>\n  <integer>63</integer>\n  \
         <key>KeepAlive</key>\n  <true/>\n",
        xml(account),
        xml(group)
    );
    let path = PathBuf::from(format!("/Library/LaunchDaemons/{name}.plist"));
    out.push(Action::WriteFile {
        path: path.clone(),
        contents: plist(name, &service_args(i), &extra),
    });
    out.push(
        Action::run("launchctl", &["bootout", &format!("system/{name}")])
            .tolerating(Tolerate::Anything),
    );
    out.push(Action::run(
        "launchctl",
        &["bootstrap", "system", &s(&path)],
    ));
    out
}

/// `broker install-service` (spec 3.12, 10.3, 11.1): its own copy of the binary; the service
/// state directory checked (refused when someone else controls it) and closed to the service
/// account (Windows: plus Administrators and SYSTEM) before the configurations `init` created
/// go in; the installing user in `[[local_user]]`; the service under its unprivileged account,
/// started at boot only once everything passed (Windows: registered for manual start until
/// the last steps; see [`install_service_undo`]).
pub fn install_service(i: &ServiceInputs) -> Vec<Action> {
    match i.os {
        Os::Windows => windows(i),
        Os::Linux => linux(i),
        Os::MacOs => macos(i),
    }
}

/// Removes the broker service and its binary (`uninstall`, `autostart off broker`); the state
/// directory, the account and the keys stay (spec 10.5). A service that is not there is fine.
pub fn uninstall_service(os: Os, binary: &Path) -> Vec<Action> {
    let name = service_name(os);
    let mut out = match os {
        Os::Windows => vec![
            Action::run("sc.exe", &["stop", name]).tolerating(Tolerate::Anything),
            Action::run("sc.exe", &["delete", name]).tolerating(Tolerate::Codes(vec![1060])),
        ],
        Os::Linux => vec![
            Action::run(
                "systemctl",
                &["disable", "--now", &format!("{name}.service")],
            )
            .tolerating(Tolerate::Anything),
            Action::RemoveFile(PathBuf::from(format!("/etc/systemd/system/{name}.service"))),
            Action::run("systemctl", &["daemon-reload"]).tolerating(Tolerate::Anything),
        ],
        Os::MacOs => vec![
            Action::run("launchctl", &["bootout", &format!("system/{name}")])
                .tolerating(Tolerate::Anything),
            Action::RemoveFile(PathBuf::from(format!(
                "/Library/LaunchDaemons/{name}.plist"
            ))),
        ],
    };
    out.push(Action::RemoveFile(binary.to_path_buf()));
    out
}
