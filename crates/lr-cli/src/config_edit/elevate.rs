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

    /// [`Elevator::run_elevated`] as the OS user `user` (`sudo -u user`) instead of the
    /// administrator; an elevator that cannot say who runs the command ignores `user`.
    fn run_elevated_as(
        &self,
        exe: &Path,
        args: &[String],
        user: Option<&str>,
    ) -> Result<i32, String> {
        let _ = user;
        self.run_elevated(exe, args)
    }
}

/// The elevation of this OS: the UAC prompt on Windows, `sudo` on Unix.
pub struct OsElevator;

impl Elevator for OsElevator {
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String> {
        self.run_elevated_as(exe, args, None)
    }

    fn run_elevated_as(
        &self,
        exe: &Path,
        args: &[String],
        user: Option<&str>,
    ) -> Result<i32, String> {
        #[cfg(windows)]
        {
            let _ = user;
            windows::run_elevated(exe, args)
        }
        #[cfg(not(windows))]
        {
            unix::run_elevated(exe, args, user)
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
    elevated_command_line_as(os, exe, args, None)
}

/// [`elevated_command_line`] that runs as the OS user `user` on Unix: `sudo -u user exe args`.
pub fn elevated_command_line_as(os: Os, exe: &Path, args: &[String], user: Option<&str>) -> String {
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
            .fold(
                match user {
                    Some(u) => format!("sudo -u {}", shell_word(u)),
                    None => String::from("sudo"),
                },
                |line, word| line + " " + &word,
            ),
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
    rerun_elevated_as(elevator, os, exe, args, None, err)
}

/// [`rerun_elevated`] as the OS user `user` where the OS has such a thing.
pub fn rerun_elevated_as(
    elevator: &dyn Elevator,
    os: Os,
    exe: &Path,
    args: &[String],
    user: Option<&str>,
    err: &mut dyn Write,
) -> i32 {
    match elevator.run_elevated_as(exe, args, user) {
        Ok(code) => code,
        Err(why) => {
            let _ = writeln!(err, "{why}");
            let line = elevated_command_line_as(os, exe, args, user);
            if !why.contains(&line) {
                let _ = writeln!(err, "run it with administrator rights: {line}");
            }
            1
        }
    }
}

/// What a probe of `sudo -n -l <command>` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoProbe {
    /// The command may run without a password.
    Allowed,
    /// A password would be needed, or the command is not allowed.
    NeedsPassword,
    /// `sudo` is not installed.
    Missing,
}

/// How `sudo` is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoRoute {
    /// Standard input is a terminal: `sudo` may ask for the password.
    Prompt,
    /// No terminal, but sudoers lets this very command run without a password: `sudo -n`.
    NoPassword,
    /// No terminal and a password is needed: the command is left to a human.
    ByHand,
    /// `sudo` is not installed.
    Missing,
}

/// The arguments of `sudo` that check, without asking for a password, whether sudoers lets
/// `exe` with `args` run without one: `-n -l <exe> <args>`.
pub fn sudo_probe_args(exe: &Path, args: &[String]) -> Vec<String> {
    [
        "-n".to_string(),
        "-l".to_string(),
        exe.display().to_string(),
    ]
    .into_iter()
    .chain(args.iter().cloned())
    .collect()
}

/// The route of `sudo` for `exe` with `args`: with a terminal `sudo` asks; without one
/// `probe` runs `sudo` with [`sudo_probe_args`] of the real command.
pub fn sudo_route(
    stdin_is_terminal: bool,
    exe: &Path,
    args: &[String],
    probe: impl FnOnce(&[String]) -> SudoProbe,
) -> SudoRoute {
    if stdin_is_terminal {
        return SudoRoute::Prompt;
    }
    match probe(&sudo_probe_args(exe, args)) {
        SudoProbe::Allowed => SudoRoute::NoPassword,
        SudoProbe::NeedsPassword => SudoRoute::ByHand,
        SudoProbe::Missing => SudoRoute::Missing,
    }
}

/// A fresh output channel of an elevated child: 128 random bits as 32 lowercase hex digits,
/// a dash and the id of this process, which serves the pipes.
pub fn new_channel() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no random bytes for a channel: {e}"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{hex}-{}", std::process::id()))
}

/// The named pipes of an output channel (standard output, then standard error) and the id of
/// the process that must serve them. Refuses anything but 32 lowercase hex digits, a dash and
/// a decimal process id, so the child never opens a path it was given.
pub fn channel_pipes(channel: &str) -> Result<([String; 2], u32), String> {
    let refused = || {
        format!("{ELEVATED_CHILD}: the channel must be 32 lowercase hex digits, a dash and a process id")
    };
    let (hex, pid) = channel.split_once('-').ok_or_else(refused)?;
    let hex_ok = hex.len() == 32
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let pid_ok = !pid.is_empty() && pid.len() <= 10 && pid.bytes().all(|b| b.is_ascii_digit());
    if !hex_ok || !pid_ok {
        return Err(refused());
    }
    let parent: u32 = pid.parse().map_err(|_| refused())?;
    let pipe = |stream: &str| format!(r"\\.\pipe\localrunner-elevate-{hex}-{stream}");
    Ok(([pipe("out"), pipe("err")], parent))
}

/// Text from an untrusted source made safe to print: invalid UTF-8 becomes U+FFFD, and
/// control characters (escape sequences included, `\r` too) and bidirectional overrides are
/// dropped; line feeds and tabs stay.
pub fn untrusted_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|&c| printable(c))
        .collect()
}

fn printable(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return true;
    }
    !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// In the elevated child: checks that the pipes of `channel` are served by the process the
/// channel names, then sends standard output and standard error there; nothing is sent to a
/// pipe served by another process. The process id comes from the channel and is not checked
/// against the real parent: after `runas` the parent of the elevated process is the
/// consent service, not the caller. Whoever starts an elevated copy with a forged channel
/// needs the user to approve UAC and already holds the user's rights, so the residual risk is
/// disclosure of this output only. Only on Windows; elsewhere the child inherits the
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

fn plain_word(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '\\' | '=')
        })
}

/// `words` as one command line for the shell of `os` (PowerShell on Windows, a POSIX shell
/// elsewhere), with the quoting of [`elevated_command_line`] for every word that needs it.
pub fn command_words(os: Os, words: &[String]) -> String {
    let quoted: Vec<String> = words
        .iter()
        .map(|w| match (plain_word(w), os) {
            (true, _) => w.clone(),
            (false, Os::Windows) => ps_literal(w),
            (false, _) => shell_word(w),
        })
        .collect();
    let line = quoted.join(" ");
    match (os, words.first()) {
        (Os::Windows, Some(first)) if !plain_word(first) => format!("& {line}"),
        _ => line,
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
    use super::{channel_pipes, child_args, elevated_command_line, new_channel, printable};
    use crate::os_service::quote::windows_command_line;
    use crate::os_service::Os;
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_CANCELLED, ERROR_IO_PENDING, ERROR_NO_DATA,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_INBOUND, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    };
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
        GetNamedPipeServerProcessId, WaitNamedPipeW, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetExitCodeProcess, GetProcessId, ResetEvent, WaitForMultipleObjects,
        WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };

    const SW_HIDE: i32 = 0;
    const PIPE_BUFFER: u32 = 64 * 1024;
    const DRAIN_LIMIT: usize = 4 * 1024 * 1024;
    const DRAIN_ROUNDS: usize = 4096;
    const CONNECT_DEADLINE: Duration = Duration::from_secs(20);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
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

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Listening,
        Reading,
        Done,
    }

    struct Text {
        sink: Box<dyn Write>,
        pending: Vec<u8>,
    }

    impl Text {
        fn push(&mut self, bytes: &[u8]) {
            self.pending.extend_from_slice(bytes);
            let mut out = String::new();
            loop {
                match std::str::from_utf8(&self.pending) {
                    Ok(text) => {
                        out.extend(text.chars().filter(|&c| printable(c)));
                        self.pending.clear();
                        break;
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        let text = std::str::from_utf8(&self.pending[..valid]).unwrap_or_default();
                        out.extend(text.chars().filter(|&c| printable(c)));
                        match e.error_len() {
                            Some(bad) => {
                                out.push('\u{FFFD}');
                                self.pending.drain(..valid + bad);
                            }
                            None => {
                                self.pending.drain(..valid);
                                break;
                            }
                        }
                    }
                }
            }
            let _ = self.sink.write_all(out.as_bytes());
        }

        fn finish(&mut self) {
            if !self.pending.is_empty() {
                let rest = std::mem::take(&mut self.pending);
                let _ = self.sink.write_all(super::untrusted_text(&rest).as_bytes());
            }
            let _ = self.sink.flush();
        }
    }

    struct Pipe {
        file: File,
        event: Owned,
        overlapped: Box<OVERLAPPED>,
        buffer: Vec<u8>,
        state: State,
        pending: bool,
        immediate: Option<Result<u32, u32>>,
        text: Text,
    }

    impl Pipe {
        fn create(name: &str, sd: &Descriptor, sink: Box<dyn Write>) -> Result<Pipe, String> {
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: 0,
            };
            let handle = unsafe {
                CreateNamedPipeW(
                    wide(name).as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
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
            let file = unsafe { File::from_raw_handle(handle as RawHandle) };
            let event = unsafe { CreateEventW(null(), 1, 0, null()) };
            if event.is_null() {
                return Err(format!(
                    "event of {name}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Pipe {
                file,
                event: Owned(event),
                overlapped: Box::new(unsafe { std::mem::zeroed() }),
                buffer: vec![0; PIPE_BUFFER as usize],
                state: State::Listening,
                pending: false,
                immediate: None,
                text: Text {
                    sink,
                    pending: Vec::new(),
                },
            })
        }

        fn handle(&self) -> HANDLE {
            self.file.as_raw_handle() as HANDLE
        }

        fn active(&self) -> bool {
            self.state != State::Done
        }

        fn ready(&self) -> bool {
            self.immediate.is_some()
        }

        fn fresh_overlapped(&mut self) {
            unsafe { ResetEvent(self.event.0) };
            *self.overlapped = unsafe { std::mem::zeroed() };
            self.overlapped.hEvent = self.event.0;
        }

        fn start(&mut self) {
            self.immediate = None;
            self.pending = false;
            match self.state {
                State::Done => {}
                State::Listening => {
                    self.fresh_overlapped();
                    let ok = unsafe { ConnectNamedPipe(self.handle(), &mut *self.overlapped) };
                    let error = unsafe { GetLastError() };
                    match (ok, error) {
                        (0, ERROR_IO_PENDING) => self.pending = true,
                        (0, ERROR_PIPE_CONNECTED | ERROR_NO_DATA) => self.immediate = Some(Ok(0)),
                        (0, _) => self.state = State::Done,
                        _ => self.immediate = Some(Ok(0)),
                    }
                }
                State::Reading => {
                    self.fresh_overlapped();
                    let ok = unsafe {
                        ReadFile(
                            self.handle(),
                            self.buffer.as_mut_ptr(),
                            self.buffer.len() as u32,
                            null_mut(),
                            &mut *self.overlapped,
                        )
                    };
                    let error = unsafe { GetLastError() };
                    match (ok, error) {
                        (0, ERROR_IO_PENDING) => self.pending = true,
                        (0, e) => self.immediate = Some(Err(e)),
                        _ => self.immediate = Some(self.result(false)),
                    }
                }
            }
        }

        fn result(&self, wait: bool) -> Result<u32, u32> {
            let mut n = 0u32;
            let ok = unsafe {
                GetOverlappedResult(self.handle(), &*self.overlapped, &mut n, wait as i32)
            };
            if ok != 0 {
                Ok(n)
            } else {
                Err(unsafe { GetLastError() })
            }
        }

        fn signaled(&self) -> bool {
            self.pending && unsafe { WaitForSingleObject(self.event.0, 0) } == WAIT_OBJECT_0
        }

        fn client_is(&self, child: u32) -> bool {
            let mut pid = 0u32;
            let known = unsafe { GetNamedPipeClientProcessId(self.handle(), &mut pid) } != 0;
            known && pid == child
        }

        fn advance(&mut self, child: u32) -> usize {
            let result = match self.immediate.take() {
                Some(r) => r,
                None => self.result(false),
            };
            self.pending = false;
            let mut read = 0;
            match self.state {
                State::Done => return 0,
                State::Listening => match result {
                    Ok(_) | Err(ERROR_PIPE_CONNECTED | ERROR_NO_DATA) => {
                        if self.client_is(child) {
                            self.state = State::Reading;
                        } else {
                            unsafe { DisconnectNamedPipe(self.handle()) };
                        }
                    }
                    Err(_) => self.state = State::Done,
                },
                State::Reading => match result {
                    Ok(n) => {
                        read = n as usize;
                        let bytes = self.buffer[..read].to_vec();
                        self.text.push(&bytes);
                    }
                    Err(_) => self.state = State::Done,
                },
            }
            if self.state == State::Done {
                self.text.finish();
            } else {
                self.start();
            }
            read
        }

        fn stop(&mut self) {
            if self.pending {
                unsafe { CancelIoEx(self.handle(), &*self.overlapped) };
                if let Ok(n) = self.result(true) {
                    if self.state == State::Reading && n > 0 {
                        let bytes = self.buffer[..n as usize].to_vec();
                        self.text.push(&bytes);
                    }
                }
                self.pending = false;
            }
            if self.state != State::Done {
                self.state = State::Done;
                self.text.finish();
            }
        }
    }

    /// The parent end of an output channel: two named pipes, one instance each, created
    /// first (`FILE_FLAG_FIRST_PIPE_INSTANCE`), local clients only, open to this user, SYSTEM
    /// and Administrators. [`OutputChannel::serve`] accepts on each pipe only the process it
    /// serves, prints what it writes as untrusted text and stops when that process exits.
    pub struct OutputChannel {
        pipes: [Pipe; 2],
    }

    impl OutputChannel {
        /// Creates the pipes of `channel`, copied to `out` and `err`; fails when a pipe of
        /// that name exists already.
        pub fn open(
            channel: &str,
            out: Box<dyn Write>,
            err: Box<dyn Write>,
        ) -> Result<OutputChannel, String> {
            let (names, _) = channel_pipes(channel)?;
            let sd = pipe_descriptor()?;
            let pipes = [
                Pipe::create(&names[0], &sd, out)?,
                Pipe::create(&names[1], &sd, err)?,
            ];
            Ok(OutputChannel { pipes })
        }

        /// Serves the pipes until `process` exits and returns its exit code, read from the
        /// process handle. A client that is not `process` is disconnected before anything it
        /// wrote is read; after the exit what is already in the pipes is drained (bounded) and
        /// every pending operation is cancelled, so this never waits on a client.
        pub fn serve(mut self, process: RawHandle) -> Result<i32, String> {
            for pipe in &mut self.pipes {
                pipe.start();
            }
            self.run(process as HANDLE)
        }

        /// Test hook of [`OutputChannel::serve`]: listens, lets `spawn` start the process,
        /// waits until that process has exited and only then handles the pipe events, so a
        /// child that connected, wrote and exited before the parent looked is reproduced.
        #[doc(hidden)]
        pub fn serve_after_exit(
            mut self,
            spawn: impl FnOnce() -> RawHandle,
        ) -> Result<i32, String> {
            for pipe in &mut self.pipes {
                pipe.start();
            }
            let process = spawn() as HANDLE;
            unsafe { WaitForSingleObject(process, INFINITE) };
            self.run(process)
        }

        fn run(&mut self, process: HANDLE) -> Result<i32, String> {
            let child = unsafe { GetProcessId(process) };
            if child == 0 {
                return Err(format!(
                    "no id of the elevated process: {}",
                    std::io::Error::last_os_error()
                ));
            }
            loop {
                self.settle(child);
                let (handles, index) = self.waits(Some(process));
                let woke = unsafe {
                    WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                };
                let k = woke.wrapping_sub(WAIT_OBJECT_0) as usize;
                if k == 0 || k >= handles.len() {
                    break;
                }
                self.pipes[index[k - 1]].advance(child);
            }
            self.drain(child);
            let mut code = 0u32;
            if unsafe { GetExitCodeProcess(process, &mut code) } == 0 {
                return Err(format!(
                    "no exit code of the elevated command: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(code as i32)
        }

        fn settle(&mut self, child: u32) {
            for pipe in &mut self.pipes {
                while pipe.active() && pipe.ready() {
                    pipe.advance(child);
                }
            }
        }

        fn waits(&self, process: Option<HANDLE>) -> (Vec<HANDLE>, Vec<usize>) {
            let mut handles: Vec<HANDLE> = process.into_iter().collect();
            let mut index = Vec::new();
            for (i, pipe) in self.pipes.iter().enumerate() {
                if pipe.active() && pipe.pending {
                    handles.push(pipe.event.0);
                    index.push(i);
                }
            }
            (handles, index)
        }

        fn drain(&mut self, child: u32) {
            for pipe in &mut self.pipes {
                if pipe.state == State::Listening && (pipe.ready() || pipe.signaled()) {
                    pipe.advance(child);
                }
                if pipe.state == State::Listening {
                    pipe.stop();
                }
            }
            let mut drained = 0usize;
            let mut rounds = 0usize;
            while drained < DRAIN_LIMIT && rounds < DRAIN_ROUNDS {
                rounds += 1;
                for pipe in &mut self.pipes {
                    while pipe.state == State::Reading
                        && pipe.ready()
                        && drained < DRAIN_LIMIT
                        && rounds < DRAIN_ROUNDS
                    {
                        rounds += 1;
                        drained += pipe.advance(child);
                    }
                }
                let (handles, index) = self.waits(None);
                if handles.is_empty() {
                    break;
                }
                let woke =
                    unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, 0) };
                if woke == WAIT_TIMEOUT {
                    break;
                }
                let k = woke.wrapping_sub(WAIT_OBJECT_0) as usize;
                if k >= handles.len() {
                    break;
                }
                drained += self.pipes[index[k]].advance(child);
            }
            for pipe in &mut self.pipes {
                pipe.stop();
            }
        }
    }

    fn open_client(name: &str) -> Result<HANDLE, String> {
        let deadline = Instant::now() + CONNECT_DEADLINE;
        let wide_name = wide(name);
        loop {
            let handle = unsafe {
                CreateFileW(
                    wide_name.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(handle);
            }
            let error = std::io::Error::last_os_error();
            let left = deadline.saturating_duration_since(Instant::now());
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || left.is_zero() {
                return Err(format!("cannot open the output channel {name}: {error}"));
            }
            unsafe { WaitNamedPipeW(wide_name.as_ptr(), left.as_millis().min(5000) as u32) };
        }
    }

    pub(super) fn attach(channel: &str) -> Result<(), String> {
        let (names, parent) = channel_pipes(channel)?;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let mut opened = Vec::new();
        for name in &names {
            let handle = Owned(open_client(name)?);
            let mut server = 0u32;
            let known = unsafe { GetNamedPipeServerProcessId(handle.0, &mut server) } != 0;
            if !known || server != parent {
                return Err(format!(
                    "the output channel {name} is not served by process {parent}: nothing was sent"
                ));
            }
            opened.push(handle);
        }
        for (handle, which) in opened
            .into_iter()
            .zip([STD_OUTPUT_HANDLE, STD_ERROR_HANDLE])
        {
            if unsafe { SetStdHandle(which, handle.0) } == 0 {
                return Err(format!(
                    "cannot redirect the output: {}",
                    std::io::Error::last_os_error()
                ));
            }
            std::mem::forget(handle);
        }
        Ok(())
    }

    enum Launch {
        Refused,
        Failed(String),
    }

    fn run_as_admin(exe: &Path, parameters: &str) -> Result<Owned, Launch> {
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
        Ok(Owned(info.hProcess))
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
        match run_as_admin(exe, &parameters) {
            Ok(process) => output.serve(process.0 as RawHandle),
            Err(Launch::Refused) => Err(format!(
                "the administrator prompt was refused: run it by hand: {line}"
            )),
            Err(Launch::Failed(why)) => Err(format!(
                "cannot start the elevated command ({why}): run it by hand: {line}"
            )),
        }
    }
}

#[cfg(not(windows))]
mod unix {
    use super::{child_args, elevated_command_line_as, sudo_route, SudoProbe, SudoRoute};
    use crate::os_service::Os;
    use std::io::IsTerminal;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn probe(args: &[String]) -> SudoProbe {
        match Command::new("sudo")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => SudoProbe::Allowed,
            Ok(_) => SudoProbe::NeedsPassword,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SudoProbe::Missing,
            Err(_) => SudoProbe::NeedsPassword,
        }
    }

    pub(super) fn run_elevated(
        exe: &Path,
        args: &[String],
        user: Option<&str>,
    ) -> Result<i32, String> {
        let line = elevated_command_line_as(Os::current(), exe, args, user);
        let marked = child_args(args, None);
        let not_installed = || format!("sudo is not installed: run the command as root: {line}");
        let mut sudo = Command::new("sudo");
        let probe = |probe_args: &[String]| match user {
            Some(u) => {
                let mut with_user = probe_args.to_vec();
                with_user.splice(2..2, ["-u".to_string(), u.to_string()]);
                probe(&with_user)
            }
            None => probe(probe_args),
        };
        match sudo_route(std::io::stdin().is_terminal(), exe, &marked, probe) {
            SudoRoute::Missing => return Err(not_installed()),
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
        if let Some(u) = user {
            sudo.args(["-u", u]);
        }
        match sudo.arg(exe).args(&marked).status() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(not_installed()),
            Err(e) => Err(format!("cannot start sudo ({e}): run it by hand: {line}")),
            Ok(status) => status
                .code()
                .ok_or_else(|| "the elevated command was stopped by a signal".into()),
        }
    }
}
