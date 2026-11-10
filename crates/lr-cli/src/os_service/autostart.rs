//! Autostart of a hub or runner at the user's logon (spec 3.12, 10): a Task Scheduler task on
//! Windows, a systemd user unit on Linux, a LaunchAgent on macOS; always under the user who
//! enabled it and without administrator rights.

use super::quote::{systemd_command_line, windows_command_line, xml};
use super::{Action, Os, Tolerate};
use std::path::{Path, PathBuf};

/// The program on Windows that starts a role without any console window: a GUI-subsystem
/// launcher next to `localrunner.exe` that starts it with a hidden console.
pub const WINDOWS_LAUNCHER: &str = "localrunnerw.exe";

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
    /// The OS user (Windows: `DOMAIN\name`, the task's logon trigger and principal).
    pub user: String,
    /// The user's principal (Windows: the SID): part of the task's name, so users of one
    /// machine never replace or remove each other's tasks.
    pub principal: String,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn role_args(a: &AutostartInputs) -> Vec<String> {
    let mut args = Vec::new();
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

/// The plist of a launchd job.
pub(crate) fn plist(label: &str, args: &[String], extra: &str) -> String {
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

/// Name of the Task Scheduler task of `a.entry` for `a`'s user: `Al.LocalRunner <entry>
/// <principal>`. The scheduler's folder is shared by the machine's users; the principal keeps
/// each user's tasks apart.
pub fn task_name(a: &AutostartInputs) -> String {
    format!("Al.LocalRunner {} {}", a.entry.name(), a.principal)
}

/// The task: started at the user's logon in the user's session with the user's ordinary
/// rights, restarted a minute after a failure, no time limit, on battery too; it runs the
/// windowless launcher.
pub fn task_xml(a: &AutostartInputs) -> String {
    let launcher = a.exe.with_file_name(WINDOWS_LAUNCHER);
    let user = xml(&a.user);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
  <RegistrationInfo>\n    <Description>Al.LocalRunner {entry}</Description>\n  </RegistrationInfo>\n\
  <Triggers>\n    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>{user}</UserId>\n    </LogonTrigger>\n  </Triggers>\n\
  <Principals>\n    <Principal id=\"Author\">\n      <UserId>{user}</UserId>\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n\
  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <AllowHardTerminate>true</AllowHardTerminate>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <AllowStartOnDemand>true</AllowStartOnDemand>\n    <Enabled>true</Enabled>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <RestartOnFailure>\n      <Interval>PT1M</Interval>\n      <Count>999</Count>\n    </RestartOnFailure>\n  </Settings>\n\
  <Actions Context=\"Author\">\n    <Exec>\n      <Command>{command}</Command>\n      <Arguments>{arguments}</Arguments>\n    </Exec>\n  </Actions>\n\
</Task>\n",
        entry = xml(&a.entry.name()),
        command = xml(&s(&launcher)),
        arguments = xml(&windows_command_line(&role_args(a))),
    )
}

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

fn with_exe(a: &AutostartInputs) -> Vec<String> {
    let mut args = vec![s(&a.exe)];
    args.extend(role_args(a));
    args
}

/// Autostart on: the task, unit or agent of `a.entry`.
pub fn autostart_on(a: &AutostartInputs) -> Vec<Action> {
    match a.os {
        Os::Windows => {
            let file = a
                .state_dir
                .join(format!("autostart-{}.xml", a.entry.name()));
            vec![
                Action::WriteUtf16 {
                    path: file.clone(),
                    contents: task_xml(a),
                },
                Action::run(
                    "schtasks",
                    &["/Create", "/TN", &task_name(a), "/XML", &s(&file), "/F"],
                ),
                Action::RemoveFile(file),
            ]
        }
        Os::Linux => {
            let (unit, path) = user_unit(a);
            let contents = format!(
                "[Unit]\nDescription=Al.LocalRunner {}\n\n[Service]\nType=simple\nExecStart={}\n\
                 Restart=on-failure\n\n[Install]\nWantedBy=default.target\n",
                a.entry.name(),
                systemd_command_line(&with_exe(a))
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
                    contents: plist(&label, &with_exe(a), ""),
                },
                Action::run("launchctl", &["bootout", &format!("{domain}/{label}")])
                    .tolerating(Tolerate::Anything),
                Action::run("launchctl", &["bootstrap", &domain, &s(&path)]),
            ]
        }
    }
}

/// Removes the autostart entry of `a.entry` and nothing else; one that does not exist is fine.
pub fn autostart_off(a: &AutostartInputs) -> Vec<Action> {
    match a.os {
        Os::Windows => {
            vec![
                Action::run("schtasks", &["/Delete", "/TN", &task_name(a), "/F"])
                    .tolerating(Tolerate::Anything),
            ]
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
