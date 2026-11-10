//! The plans: pure functions from inputs to [`Action`]s, one per OS, so each can be checked on
//! any machine without touching it.

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

/// The role an autostart entry starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// The hub of the user.
    Hub,
    /// A runner; `None` for the default runner (`runner run` without a name).
    Runner(Option<String>),
}

impl Entry {
    /// The entry's name: `hub`, `runner` or `runner-<name>`.
    pub fn name(&self) -> String {
        match self {
            Entry::Hub => "hub".into(),
            Entry::Runner(None) => "runner".into(),
            Entry::Runner(Some(n)) => format!("runner-{n}"),
        }
    }
}

/// What `autostart on|off` needs for a hub or runner (spec 10: under the user who enables it).
#[derive(Debug, Clone)]
pub struct AutostartInputs {
    /// Target OS.
    pub os: Os,
    /// The binary to start.
    pub exe: PathBuf,
    /// What it starts.
    pub entry: Entry,
    /// State directory of the installation.
    pub state_dir: PathBuf,
    /// Configuration directory, when it is not the state directory.
    pub config_dir: Option<PathBuf>,
    /// Home directory of the user (systemd user units, LaunchAgents).
    pub home: PathBuf,
    /// Numeric uid of the user (macOS `gui/<uid>` domain).
    pub uid: u32,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn role_args(a: &AutostartInputs) -> Vec<String> {
    let mut args = vec![s(&a.exe)];
    match &a.entry {
        Entry::Hub => args.extend(["hub", "run"].map(String::from)),
        Entry::Runner(name) => {
            args.extend(["runner", "run"].map(String::from));
            if let Some(n) = name {
                args.extend(["--name".to_string(), n.clone()]);
            }
        }
    }
    args.extend([
        "--background".to_string(),
        "--state-dir".into(),
        s(&a.state_dir),
    ]);
    if let Some(c) = &a.config_dir {
        args.extend(["--config-dir".to_string(), s(c)]);
    }
    args
}

fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains([' ', '\t']) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn systemd_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn plist(label: &str, args: &[String], extra: &str) -> String {
    let items: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml(a)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n{extra}</dict>\n</plist>\n",
        xml(label)
    )
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn user_unit(a: &AutostartInputs) -> (String, PathBuf) {
    let unit = format!("al-localrunner-{}.service", a.entry.name());
    let path = a.home.join(".config/systemd/user").join(&unit);
    (unit, path)
}

fn agent(a: &AutostartInputs) -> (String, PathBuf) {
    let label = format!("com.al-localrunner.{}", a.entry.name());
    let path = a
        .home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    (label, path)
}

/// Autostart of a hub or runner at the user's logon: the per-user `Run` key on Windows (no
/// administrator rights needed), a systemd user unit on Linux, a LaunchAgent on macOS.
pub fn autostart_on(a: &AutostartInputs) -> Vec<Action> {
    let args = role_args(a);
    match a.os {
        Os::Windows => {
            let value = format!("Al.LocalRunner {}", a.entry.name());
            let line = windows_command_line(&args);
            vec![Action::run(
                "reg",
                &[
                    "add", RUN_KEY, "/v", &value, "/t", "REG_SZ", "/d", &line, "/f",
                ],
            )]
        }
        Os::Linux => {
            let (unit, path) = user_unit(a);
            let contents = format!(
                "[Unit]\nDescription=Al.LocalRunner {}\n\n[Service]\nType=simple\nExecStart={}\n\
                 Restart=on-failure\n\n[Install]\nWantedBy=default.target\n",
                a.entry.name(),
                systemd_command_line(&args)
            );
            vec![
                Action::WriteFile { path, contents },
                Action::run("systemctl", &["--user", "daemon-reload"]),
                Action::run("systemctl", &["--user", "enable", &unit]),
            ]
        }
        Os::MacOs => {
            let (label, path) = agent(a);
            let domain = format!("gui/{}", a.uid);
            vec![
                Action::WriteFile {
                    path: path.clone(),
                    contents: plist(&label, &args, ""),
                },
                Action::run("launchctl", &["bootout", &format!("{domain}/{label}")])
                    .tolerating(Tolerate::Anything),
                Action::run("launchctl", &["bootstrap", &domain, &s(&path)]),
            ]
        }
    }
}

/// Removes the autostart entry; an entry that does not exist is fine.
pub fn autostart_off(a: &AutostartInputs) -> Vec<Action> {
    match a.os {
        Os::Windows => {
            let value = format!("Al.LocalRunner {}", a.entry.name());
            vec![Action::run("reg", &["delete", RUN_KEY, "/v", &value, "/f"])
                .tolerating(Tolerate::Anything)]
        }
        Os::Linux => {
            let (unit, path) = user_unit(a);
            vec![
                Action::run("systemctl", &["--user", "disable", &unit])
                    .tolerating(Tolerate::Anything),
                Action::RemoveFile(path),
                Action::run("systemctl", &["--user", "daemon-reload"])
                    .tolerating(Tolerate::Anything),
            ]
        }
        Os::MacOs => {
            let (label, path) = agent(a);
            vec![
                Action::run("launchctl", &["bootout", &format!("gui/{}/{label}", a.uid)])
                    .tolerating(Tolerate::Anything),
                Action::RemoveFile(path),
            ]
        }
    }
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

fn common_files(i: &ServiceInputs) -> Vec<Action> {
    let mut out = vec![
        Action::Copy {
            from: i.exe.clone(),
            to: i.binary.clone(),
        },
        Action::CreateDir(i.root.clone()),
    ];
    for f in ["policy.toml", "logging.toml"] {
        out.push(Action::CopyIfMissing {
            from: i.configs.join(f),
            to: i.root.join(f),
        });
    }
    out.push(Action::EnsureLocalUser {
        policy: i.root.join("policy.toml"),
        user: i.user.clone(),
    });
    out
}

/// `broker install-service` (spec 3.12, 10.3, 11.1): its own copy of the binary, the service
/// state directory with the configurations `init` created, the installing user in
/// `[[local_user]]`, the service under its unprivileged account, the directory owned by that
/// account (administrators keep full access on Windows), started at boot.
pub fn install_service(i: &ServiceInputs) -> Vec<Action> {
    let name = service_name(i.os);
    let account = service_account(i.os);
    let group = service_group(i.os);
    let root = s(&i.root);
    let mut out = Vec::new();
    match i.os {
        Os::Windows => {
            out.push(Action::run("net", &["stop", name]).tolerating(Tolerate::Anything));
            out.extend(common_files(i));
            let bin_path = windows_command_line(&service_args(i));
            let params = |verb: &str| -> Vec<String> {
                [
                    verb,
                    name,
                    "binPath=",
                    &bin_path,
                    "start=",
                    "auto",
                    "obj=",
                    account,
                    "DisplayName=",
                    "Al.LocalRunner broker",
                ]
                .map(String::from)
                .to_vec()
            };
            let create: Vec<String> = params("create");
            let config: Vec<String> = params("config");
            out.push(Action::Run {
                program: "sc.exe".into(),
                args: create,
                tolerate: Tolerate::Codes(vec![1073]),
            });
            out.push(Action::Run {
                program: "sc.exe".into(),
                args: config,
                tolerate: Tolerate::Nothing,
            });
            out.push(Action::run(
                "sc.exe",
                &[
                    "failure",
                    name,
                    "reset=",
                    "86400",
                    "actions=",
                    "restart/5000",
                ],
            ));
            out.push(Action::run(
                "icacls",
                &[&root, "/setowner", account, "/T", "/C"],
            ));
            let grant = format!("{account}:(OI)(CI)F");
            out.push(Action::run(
                "icacls",
                &[
                    &root,
                    "/inheritance:r",
                    "/grant:r",
                    &grant,
                    "*S-1-5-32-544:(OI)(CI)F",
                    "*S-1-5-18:(OI)(CI)F",
                    "/T",
                    "/C",
                ],
            ));
            out.push(
                Action::run("sc.exe", &["start", name]).tolerating(Tolerate::Codes(vec![1056])),
            );
        }
        Os::Linux => {
            out.push(
                Action::run("groupadd", &["--system", group]).tolerating(Tolerate::Codes(vec![9])),
            );
            out.push(
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
            );
            out.push(Action::run("usermod", &["-aG", group, &i.user]));
            out.extend(common_files(i));
            out.push(Action::run(
                "chown",
                &["-R", &format!("{account}:{group}"), &root],
            ));
            out.push(Action::run("chmod", &["0750", &root]));
            let unit = format!(
                "[Unit]\nDescription=Al.LocalRunner pool broker\nAfter=network-online.target\n\
                 Wants=network-online.target\n\n[Service]\nType=simple\nUser={account}\nGroup={group}\n\
                 ExecStart={}\nRuntimeDirectory=al-localrunner\nRuntimeDirectoryMode=0750\n\
                 Restart=on-failure\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n",
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
        }
        Os::MacOs => {
            out.push(Action::EnsureMacAccount {
                name: account.into(),
                home: i.root.clone(),
            });
            out.push(Action::run(
                "dseditgroup",
                &["-o", "edit", "-a", &i.user, "-t", "user", group],
            ));
            out.extend(common_files(i));
            out.push(Action::run(
                "chown",
                &["-R", &format!("{account}:{group}"), &root],
            ));
            out.push(Action::run("chmod", &["0750", &root]));
            let extra = format!(
                "  <key>UserName</key>\n  <string>{}</string>\n  <key>GroupName</key>\n  \
                 <string>{}</string>\n  <key>KeepAlive</key>\n  <true/>\n",
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
        }
    }
    out
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
