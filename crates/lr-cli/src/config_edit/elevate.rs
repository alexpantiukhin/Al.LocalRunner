//! Re-running a command with administrator rights when the file it writes needs them.

use crate::os_service::quote::{shell_word, windows_command_line};
use crate::os_service::Os;
use std::io::Write;
use std::path::Path;

/// The hidden top-level flag of the elevated child, always the first argument after the
/// program: the child does not elevate again and skips the diff the parent already showed.
/// On Windows it carries the output channel, `--elevated-child=<channel>`.
pub const ELEVATED_CHILD: &str = "--elevated-child";

/// Runs a program with administrator rights.
pub trait Elevator {
    /// Runs `exe` with `args` elevated, marked as the elevated child, and returns the exit
    /// code of the child. A refused prompt, a missing elevation tool or a session that cannot
    /// ask for rights is an error that says so and names the command to run by hand.
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String>;
}

/// The elevation of this OS: the UAC prompt on Windows, `sudo` on Unix.
pub struct OsElevator;

impl Elevator for OsElevator {
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String> {
        #[cfg(windows)]
        {
            windows::run_elevated(exe, args)
        }
        #[cfg(not(windows))]
        {
            unix::run_elevated(exe, args)
        }
    }
}

fn is_marker(arg: &str) -> bool {
    arg == ELEVATED_CHILD
        || arg
            .strip_prefix(ELEVATED_CHILD)
            .is_some_and(|rest| rest.starts_with('='))
}

/// `args` with the child marker as the first argument: `--elevated-child`, or
/// `--elevated-child=<channel>` with a channel. A marker already in the first place is
/// replaced; an argument equal to the marker anywhere else is a value and stays as it is.
pub fn child_args(args: &[String], channel: Option<&str>) -> Vec<String> {
    let rest = match args.first() {
        Some(first) if is_marker(first) => &args[1..],
        _ => args,
    };
    let marker = match channel {
        Some(c) => format!("{ELEVATED_CHILD}={c}"),
        None => ELEVATED_CHILD.to_string(),
    };
    std::iter::once(marker)
        .chain(rest.iter().cloned())
        .collect()
}

/// The command a user runs to start `exe` with `args` elevated, printed when this session
/// cannot elevate: a PowerShell `Start-Process -Verb RunAs` line on Windows, `sudo` on Unix.
pub fn elevated_command_line(os: Os, exe: &Path, args: &[String]) -> String {
    let exe = exe.display().to_string();
    match os {
        Os::Windows => {
            let mut line = format!(
                "Start-Process -Verb RunAs -Wait -FilePath {}",
                ps_literal(&exe)
            );
            if !args.is_empty() {
                line.push_str(" -ArgumentList ");
                line.push_str(&ps_literal(&windows_command_line(args)));
            }
            line
        }
        _ => std::iter::once(exe.as_str())
            .chain(args.iter().map(String::as_str))
            .map(shell_word)
            .fold(String::from("sudo"), |line, word| line + " " + &word),
    }
}

/// Runs `exe` with `args` through `elevator` and returns the exit code of the child. When
/// elevation fails, writes why and the command to run by hand to `err` and returns 1.
pub fn rerun_elevated(
    elevator: &dyn Elevator,
    os: Os,
    exe: &Path,
    args: &[String],
    err: &mut dyn Write,
) -> i32 {
    match elevator.run_elevated(exe, args) {
        Ok(code) => code,
        Err(why) => {
            let _ = writeln!(err, "{why}");
            let line = elevated_command_line(os, exe, args);
            if !why.contains(&line) {
                let _ = writeln!(err, "run it with administrator rights: {line}");
            }
            1
        }
    }
}

/// How `sudo` is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoRoute {
    /// Standard input is a terminal: `sudo` may ask for the password.
    Prompt,
    /// No terminal, but `sudo -n` needs no password.
    NoPassword,
    /// No terminal and a password is needed: the command is left to a human.
    ByHand,
}

/// The route of `sudo` for a session whose standard input is a terminal or not;
/// `passwordless` is asked only without a terminal.
pub fn sudo_route(stdin_is_terminal: bool, passwordless: impl FnOnce() -> bool) -> SudoRoute {
    if stdin_is_terminal {
        SudoRoute::Prompt
    } else if passwordless() {
        SudoRoute::NoPassword
    } else {
        SudoRoute::ByHand
    }
}

/// A fresh output channel of an elevated child: 128 random bits as 32 lowercase hex digits.
pub fn new_channel() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no random bytes for a channel: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The named pipes of an output channel, standard output then standard error; refuses a
/// channel that is not 32 lowercase hex digits, so the child never opens a path it was given.
pub fn channel_pipes(channel: &str) -> Result<[String; 2], String> {
    let valid = channel.len() == 32
        && channel
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(format!(
            "{ELEVATED_CHILD}: the channel must be 32 lowercase hex digits"
        ));
    }
    let pipe = |stream: &str| format!(r"\\.\pipe\localrunner-elevate-{channel}-{stream}");
    Ok([pipe("out"), pipe("err")])
}

/// In the elevated child: sends standard output and standard error to the pipes of
/// `channel`, which the parent created. Only on Windows; elsewhere the child inherits the
/// terminal and a channel is refused.
pub fn attach_elevated_output(channel: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::attach(channel)
    }
    #[cfg(not(windows))]
    {
        Err(format!(
            "{ELEVATED_CHILD}={channel}: an output channel exists only on Windows"
        ))
    }
}

fn ps_literal(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(windows)]
pub use windows::OutputChannel;

#[cfg(windows)]
mod windows {
    use super::{channel_pipes, child_args, elevated_command_line, new_channel};
    use crate::os_service::quote::windows_command_line;
    use crate::os_service::Os;
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::thread::JoinHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_CANCELLED, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
        GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
        SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    };
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };

    const SW_HIDE: i32 = 0;
    const PIPE_BUFFER: u32 = 64 * 1024;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }

    fn pipe_descriptor() -> Result<Descriptor, String> {
        let user = lr_net::current_os_principal().map_err(|e| e.to_string())?;
        let sddl = wide(&format!("D:P(A;;GA;;;{user})(A;;GA;;;BA)(A;;GA;;;SY)"));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(format!(
                "security descriptor of the output channel: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Descriptor(sd))
    }

    fn create_pipe(name: &str, sd: &Descriptor) -> Result<File, String> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateNamedPipeW(
                wide(name).as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                PIPE_BUFFER,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("{name}: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }

    fn forward(mut pipe: File, mut sink: Box<dyn Write + Send>) {
        let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle() as HANDLE, null_mut()) };
        if connected == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_CONNECTED && error != ERROR_NO_DATA {
                return;
            }
        }
        let _ = std::io::copy(&mut pipe, &mut sink);
        let _ = sink.flush();
    }

    /// The parent end of an output channel: two named pipes, one instance each, created
    /// first (`FILE_FLAG_FIRST_PIPE_INSTANCE`), local clients only, open to this user, SYSTEM
    /// and Administrators. Whatever the child writes is copied to the sinks as it comes.
    pub struct OutputChannel {
        names: [String; 2],
        readers: Vec<JoinHandle<()>>,
    }

    impl OutputChannel {
        /// Creates the pipes of `channel` and starts copying them to `out` and `err`; fails
        /// when a pipe of that name exists already.
        pub fn open(
            channel: &str,
            out: Box<dyn Write + Send>,
            err: Box<dyn Write + Send>,
        ) -> Result<OutputChannel, String> {
            let names = channel_pipes(channel)?;
            let sd = pipe_descriptor()?;
            let pipes = [create_pipe(&names[0], &sd)?, create_pipe(&names[1], &sd)?];
            let readers = pipes
                .into_iter()
                .zip([out, err])
                .map(|(pipe, sink)| std::thread::spawn(move || forward(pipe, sink)))
                .collect();
            Ok(OutputChannel { names, readers })
        }

        /// Call after the child has exited: releases a pipe the child never opened and waits
        /// until everything it wrote has reached the sinks.
        pub fn finish(self) {
            for name in &self.names {
                let _ = std::fs::OpenOptions::new().write(true).open(name);
            }
            for reader in self.readers {
                let _ = reader.join();
            }
        }
    }

    pub(super) fn attach(channel: &str) -> Result<(), String> {
        let names = channel_pipes(channel)?;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        for (name, which) in names.iter().zip([STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]) {
            let handle = unsafe {
                CreateFileW(
                    wide(name).as_ptr(),
                    GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(format!(
                    "cannot open the output channel {name}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if unsafe { SetStdHandle(which, handle) } == 0 {
                return Err(format!(
                    "cannot redirect the output to {name}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(())
    }

    enum Launch {
        Refused,
        Failed(String),
    }

    fn run_as_admin(exe: &Path, parameters: &str) -> Result<i32, Launch> {
        let verb = wide("runas");
        let file = wide(&exe.display().to_string());
        let parameters = wide(parameters);
        let directory = std::env::current_dir()
            .ok()
            .map(|d| wide(&d.display().to_string()));
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.lpDirectory = directory.as_ref().map_or(null(), |d| d.as_ptr());
        info.nShow = SW_HIDE;
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_CANCELLED {
                return Err(Launch::Refused);
            }
            return Err(Launch::Failed(
                std::io::Error::from_raw_os_error(error as i32).to_string(),
            ));
        }
        if info.hProcess.is_null() {
            return Err(Launch::Failed("no process handle".into()));
        }
        let mut code = 0u32;
        let read = unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let read = GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            read
        };
        if read == 0 {
            return Err(Launch::Failed(
                "no exit code of the elevated command".into(),
            ));
        }
        Ok(code as i32)
    }

    pub(super) fn run_elevated(exe: &Path, args: &[String]) -> Result<i32, String> {
        let line = elevated_command_line(Os::Windows, exe, args);
        let channel = new_channel()?;
        let output = OutputChannel::open(
            &channel,
            Box::new(std::io::stdout()),
            Box::new(std::io::stderr()),
        )?;
        let parameters = windows_command_line(&child_args(args, Some(&channel)));
        let result = run_as_admin(exe, &parameters);
        output.finish();
        result.map_err(|e| match e {
            Launch::Refused => {
                format!("the administrator prompt was refused: run it by hand: {line}")
            }
            Launch::Failed(why) => {
                format!("cannot start the elevated command ({why}): run it by hand: {line}")
            }
        })
    }
}

#[cfg(not(windows))]
mod unix {
    use super::{child_args, elevated_command_line, sudo_route, SudoRoute};
    use crate::os_service::Os;
    use std::io::IsTerminal;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn not_installed(line: &str) -> String {
        format!("sudo is not installed: run the command as root: {line}")
    }

    pub(super) fn run_elevated(exe: &Path, args: &[String]) -> Result<i32, String> {
        let line = elevated_command_line(Os::current(), exe, args);
        let mut missing = false;
        let route = sudo_route(std::io::stdin().is_terminal(), || {
            match Command::new("sudo")
                .args(["-n", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) => status.success(),
                Err(e) => {
                    missing = e.kind() == std::io::ErrorKind::NotFound;
                    false
                }
            }
        });
        if missing {
            return Err(not_installed(&line));
        }
        let mut sudo = Command::new("sudo");
        match route {
            SudoRoute::ByHand => {
                return Err(format!(
                    "sudo needs a password and there is no terminal to ask for it: run it by hand: {line}"
                ))
            }
            SudoRoute::NoPassword => {
                sudo.arg("-n");
            }
            SudoRoute::Prompt => {}
        }
        match sudo.arg(exe).args(child_args(args, None)).status() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_installed(&line)),
            Err(e) => Err(format!("cannot start sudo ({e}): run it by hand: {line}")),
            Ok(status) => status
                .code()
                .ok_or_else(|| "the elevated command was stopped by a signal".into()),
        }
    }
}
