//! What the OS does for LocalRunner (spec 3.12, 10, 10.3): the broker service under its own
//! unprivileged account and the optional autostart of roles. Every change to the system is an
//! [`Action`] built by the pure functions of [`plans`] and carried out by a [`System`]: the real
//! one ([`OsSystem`]) or one that only prints ([`DryRun`]), so tests never touch the machine.

mod actions;
pub mod autostart;
pub mod plans;
pub mod programs;
pub mod quote;
pub mod trust;
#[cfg(windows)]
pub mod windows_host;

pub use actions::{
    apply_all, apply_or_undo, ensure_local_user, path_script, profile_line, profile_without,
    DryRun, OsSystem,
};

use lr_net::ServerIdentity;
use std::fmt;
use std::path::PathBuf;

/// The OS a plan is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Windows: a Windows service, the per-user `Run` key.
    Windows,
    /// Linux: a system unit and user units of systemd.
    Linux,
    /// macOS: a LaunchDaemon and LaunchAgents.
    MacOs,
}

impl Os {
    /// The OS of this build.
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Name of the broker service: the Windows service, the systemd unit (without `.service`),
/// the launchd label.
pub fn service_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "AlLocalRunnerBroker",
        Os::Linux => "al-localrunner-broker",
        Os::MacOs => "com.al-localrunner.broker",
    }
}

/// The unprivileged account the broker service runs under (spec 3.12): the virtual service
/// account on Windows (no password, not `SYSTEM`), a system user elsewhere.
pub fn service_account(os: Os) -> &'static str {
    match os {
        Os::Windows => r"NT SERVICE\AlLocalRunnerBroker",
        Os::Linux => "al-localrunner",
        Os::MacOs => "_allocalrunner",
    }
}

/// The group whose members reach the service's socket (Unix; spec 10, table "IPC").
pub fn service_group(os: Os) -> &'static str {
    match os {
        Os::MacOs => "_allocalrunner",
        _ => "al-localrunner",
    }
}

/// Where the broker service's own copy of the binary lives: outside any user's reach, so the
/// service never runs a file an ordinary user can replace.
pub fn service_binary(os: Os) -> PathBuf {
    match os {
        Os::Windows => {
            let files =
                std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into());
            PathBuf::from(files)
                .join("Al.LocalRunner")
                .join("localrunner.exe")
        }
        _ => PathBuf::from("/usr/local/libexec/al-localrunner/localrunner"),
    }
}

/// The servers a CLI or hub accepts on the broker's IPC (checked before anything is sent):
/// this OS user, the administrators, and the broker service's account when it exists.
pub fn broker_servers() -> Result<ServerIdentity, String> {
    let me = ServerIdentity::current_user()
        .map_err(|e| e.to_string())?
        .or_admins();
    Ok(
        match lr_net::account_principal(service_account(Os::current())) {
            Ok(service) => me.or(service),
            Err(_) => me,
        },
    )
}

/// For a role the OS started at logon: on Windows, a console window only this process uses
/// is dropped, and the process gets a hidden console its steps share (spec 10); elsewhere
/// nothing.
pub fn background_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
        let mut pids = [0u32; 2];
        let attached = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 2) };
        if attached == 1 {
            unsafe {
                FreeConsole();
            }
        }
        let _ = lr_exec::ensure_console();
    }
}

/// Whether the broker service is registered with the OS.
pub fn service_installed(os: Os) -> bool {
    match os {
        Os::Windows => programs::program_path("sc.exe").is_ok_and(|sc| {
            std::process::Command::new(sc)
                .args(["query", service_name(os)])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }),
        Os::Linux => {
            PathBuf::from(format!("/etc/systemd/system/{}.service", service_name(os))).exists()
        }
        Os::MacOs => {
            PathBuf::from(format!("/Library/LaunchDaemons/{}.plist", service_name(os))).exists()
        }
    }
}

/// The user an installation is for (spec 3.8): the one who ran `sudo`; for an elevated
/// Windows process (UAC, possibly with another administrator's credentials) the user logged on
/// to the session; else the user of this process.
pub fn choose_user(
    sudo_user: Option<String>,
    session_user: Option<String>,
    current: String,
    elevated: bool,
) -> String {
    let given = |u: Option<String>| u.filter(|u| !u.is_empty());
    if let Some(u) = given(sudo_user) {
        return u;
    }
    match given(session_user) {
        Some(u) if elevated => u,
        _ => current,
    }
}

#[cfg(windows)]
fn session_user() -> Option<String> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName,
        WTS_CURRENT_SESSION, WTS_INFO_CLASS,
    };
    let query = |class: WTS_INFO_CLASS| -> Option<String> {
        let mut buf: windows_sys::core::PWSTR = std::ptr::null_mut();
        let mut bytes = 0u32;
        let ok = unsafe {
            WTSQuerySessionInformationW(
                std::ptr::null_mut(),
                WTS_CURRENT_SESSION,
                class,
                &mut buf,
                &mut bytes,
            )
        };
        if ok == 0 || buf.is_null() {
            return None;
        }
        let mut len = 0;
        while unsafe { *buf.add(len) } != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(buf, len) });
        unsafe { WTSFreeMemory(buf as *mut std::ffi::c_void) };
        Some(text)
    };
    let user = query(WTSUserName).filter(|u| !u.is_empty())?;
    Some(match query(WTSDomainName).filter(|d| !d.is_empty()) {
        Some(domain) => format!("{domain}\\{user}"),
        None => user,
    })
}

#[cfg(not(windows))]
fn session_user() -> Option<String> {
    None
}

/// The OS user who asked (see [`choose_user`]).
pub fn original_user() -> Result<String, String> {
    let sudo = if cfg!(windows) {
        None
    } else {
        std::env::var_os("SUDO_USER").map(|u| u.to_string_lossy().into_owned())
    };
    let current = lr_net::current_os_user().map_err(|e| e.to_string())?;
    Ok(choose_user(
        sudo,
        session_user(),
        current,
        crate::host::is_elevated(),
    ))
}

/// The user's home directory.
pub fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "no home directory (HOME)".to_string())
}

/// The numeric uid of this process (0 on Windows).
pub fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// How a failed program counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tolerate {
    /// Only exit code 0 succeeds.
    Nothing,
    /// These exit codes succeed too (an account that exists, a service already stopped).
    Codes(Vec<i32>),
    /// Any outcome is fine (best-effort cleanup).
    Anything,
}

/// One change to the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Runs a program without a shell.
    Run {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// Which failures are fine.
        tolerate: Tolerate,
    },
    /// Creates a directory and its parents.
    CreateDir(PathBuf),
    /// Writes a file, replacing it.
    WriteFile {
        /// The file.
        path: PathBuf,
        /// Its contents.
        contents: String,
    },
    /// Copies a file unless the target exists or the source is missing; an existing target that
    /// someone other than the administrators or the service controls is refused.
    CopyIfMissing {
        /// Source.
        from: PathBuf,
        /// Target.
        to: PathBuf,
    },
    /// Copies a file, replacing the target.
    Copy {
        /// Source.
        from: PathBuf,
        /// Target.
        to: PathBuf,
    },
    /// Removes a file if it exists.
    RemoveFile(PathBuf),
    /// Removes a directory tree if it exists.
    RemoveDirAll(PathBuf),
    /// Adds `[[local_user]] name = user` to a `policy.toml` that does not list the user.
    EnsureLocalUser {
        /// The `policy.toml`.
        policy: PathBuf,
        /// OS user name.
        user: String,
    },
    /// Puts a directory on the user's `PATH` (Windows: the user environment; Unix: a line in
    /// `~/.profile`).
    AddToUserPath(PathBuf),
    /// Takes a directory off the user's `PATH` again.
    RemoveFromUserPath(PathBuf),
    /// Writes a file as UTF-16 with a byte order mark (Task Scheduler XML), replacing it.
    WriteUtf16 {
        /// The file.
        path: PathBuf,
        /// Its contents.
        contents: String,
    },
    /// Checks the broker service's state directory (refusing one that a link, another owner or
    /// another writer controls), creates it when missing and closes it to the service account
    /// and the administrators.
    SecureServiceDir {
        /// The directory.
        path: PathBuf,
        /// The service account.
        account: String,
    },
    /// Creates the macOS service account and group when missing.
    EnsureMacAccount {
        /// Account and group name.
        name: String,
        /// Its home directory.
        home: PathBuf,
    },
}

impl Action {
    /// A program run whose failure stops the plan.
    pub fn run(program: &str, args: &[&str]) -> Action {
        Action::Run {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            tolerate: Tolerate::Nothing,
        }
    }

    /// The same run, with these failures being fine.
    pub fn tolerating(self, t: Tolerate) -> Action {
        match self {
            Action::Run { program, args, .. } => Action::Run {
                program,
                args,
                tolerate: t,
            },
            other => other,
        }
    }
}

fn quoted(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '"', '\'']) {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Run { program, args, .. } => {
                write!(f, "run: {}", quoted(program))?;
                for a in args {
                    write!(f, " {}", quoted(a))?;
                }
                Ok(())
            }
            Action::CreateDir(p) => write!(f, "create directory {}", p.display()),
            Action::WriteFile { path, .. } => write!(f, "write {}", path.display()),
            Action::CopyIfMissing { from, to } => {
                write!(f, "copy {} to {} if missing", from.display(), to.display())
            }
            Action::Copy { from, to } => write!(f, "copy {} to {}", from.display(), to.display()),
            Action::RemoveFile(p) => write!(f, "remove {}", p.display()),
            Action::RemoveDirAll(p) => write!(f, "remove directory {}", p.display()),
            Action::EnsureLocalUser { policy, user } => {
                write!(f, "add [[local_user]] {user} to {}", policy.display())
            }
            Action::AddToUserPath(p) => write!(f, "add {} to PATH", p.display()),
            Action::RemoveFromUserPath(p) => write!(f, "remove {} from PATH", p.display()),
            Action::WriteUtf16 { path, .. } => write!(f, "write {}", path.display()),
            Action::SecureServiceDir { path, account } => write!(
                f,
                "check {} and close it to {account} and the administrators",
                path.display()
            ),
            Action::EnsureMacAccount { name, .. } => write!(f, "create account {name}"),
        }
    }
}

/// Carries out actions.
pub trait System {
    /// Carries out one action.
    fn apply(&self, action: &Action) -> Result<(), String>;
}
