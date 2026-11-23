//! Re-running a command with administrator rights when the file it writes needs them.

use crate::os_service::quote::{shell_word, windows_command_line};
use crate::os_service::Os;
use std::io::Write;
use std::path::Path;

/// The hidden flag of the elevated child: it does not elevate again and skips the diff the
/// parent already showed.
pub const ELEVATED_CHILD: &str = "--elevated-child";

/// Runs a program with administrator rights.
pub trait Elevator {
    /// Runs `exe` with `args` elevated and returns the exit code of the child. A refused
    /// prompt or a missing elevation tool is an error that says so.
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String>;
}

/// The elevation of this OS: the UAC prompt on Windows, `sudo` on Unix.
pub struct OsElevator;

impl Elevator for OsElevator {
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String> {
        if cfg!(windows) {
            run_windows(exe, args)
        } else {
            run_sudo(exe, args)
        }
    }
}

/// `args` with [`ELEVATED_CHILD`] at the end, added only when it is not there yet.
pub fn child_args(args: &[String]) -> Vec<String> {
    let mut out = args.to_vec();
    if !out.iter().any(|a| a == ELEVATED_CHILD) {
        out.push(ELEVATED_CHILD.to_string());
    }
    out
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

/// Runs `exe` with [`child_args`] through `elevator` and returns the exit code of the
/// child. When elevation fails, writes why and the command to run by hand to `err` and
/// returns 1.
pub fn rerun_elevated(
    elevator: &dyn Elevator,
    os: Os,
    exe: &Path,
    args: &[String],
    err: &mut dyn Write,
) -> i32 {
    match elevator.run_elevated(exe, &child_args(args)) {
        Ok(code) => code,
        Err(why) => {
            let _ = writeln!(err, "{why}");
            let _ = writeln!(
                err,
                "run it with administrator rights: {}",
                elevated_command_line(os, exe, args)
            );
            1
        }
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

/// `script` as the base64 UTF-16LE text `powershell -EncodedCommand` reads.
pub fn encoded_command(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The PowerShell script the elevated PowerShell runs: it starts `exe` with `args` and writes
/// its output to `out.txt` and `err.txt` and its exit code to `code.txt` in `dir`.
pub fn windows_child_script(exe: &Path, args: &[String], dir: &Path) -> String {
    let file = |name: &str| ps_literal(&dir.join(name).display().to_string());
    let (out, err, code) = (file("out.txt"), file("err.txt"), file("code.txt"));
    format!(
        "$ProgressPreference = 'SilentlyContinue'; try {{ \
         $i = New-Object System.Diagnostics.ProcessStartInfo; \
         $i.FileName = {exe}; $i.Arguments = {args}; \
         $i.UseShellExecute = $false; $i.CreateNoWindow = $true; \
         $i.RedirectStandardOutput = $true; $i.RedirectStandardError = $true; \
         $i.StandardOutputEncoding = [System.Text.Encoding]::UTF8; \
         $i.StandardErrorEncoding = [System.Text.Encoding]::UTF8; \
         $p = [System.Diagnostics.Process]::Start($i); \
         $o = $p.StandardOutput.ReadToEndAsync(); $e = $p.StandardError.ReadToEndAsync(); \
         $p.WaitForExit(); \
         [System.IO.File]::WriteAllText({out}, $o.Result); \
         [System.IO.File]::WriteAllText({err}, $e.Result); \
         [System.IO.File]::WriteAllText({code}, [string]$p.ExitCode); exit 0 \
         }} catch {{ [System.IO.File]::WriteAllText({err}, $_.Exception.Message); exit 1 }}",
        exe = ps_literal(&exe.display().to_string()),
        args = ps_literal(&windows_command_line(args)),
    )
}

fn prompt_script(child: &str) -> String {
    format!(
        "$ProgressPreference = 'SilentlyContinue'; try {{ Start-Process -Verb RunAs -Wait -WindowStyle Hidden -FilePath 'powershell.exe' \
         -ArgumentList '-NoProfile -NonInteractive -EncodedCommand {}' -ErrorAction Stop | Out-Null; \
         exit 0 }} catch {{ [Console]::Out.Write($_.Exception.Message); exit 1 }}",
        encoded_command(child)
    )
}

fn scratch_dir() -> Result<std::path::PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "localrunner-elevate-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).map_err(|e| {
        format!(
            "cannot create {} for the elevated output: {e}",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn run_windows(exe: &Path, args: &[String]) -> Result<i32, String> {
    let dir = scratch_dir()?;
    let result = run_windows_in(exe, args, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_windows_in(exe: &Path, args: &[String], dir: &Path) -> Result<i32, String> {
    let script = prompt_script(&windows_child_script(exe, args, dir));
    let prompt = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(encoded_command(&script))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot start PowerShell for the administrator prompt: {e}"))?;
    if !prompt.status.success() {
        let why = String::from_utf8_lossy(&prompt.stdout);
        return Err(format!(
            "the administrator prompt was refused or failed: {}",
            why.trim()
        ));
    }
    let read = |name: &str| std::fs::read(dir.join(name)).ok();
    let text =
        |bytes: Option<Vec<u8>>| String::from_utf8_lossy(&bytes.unwrap_or_default()).into_owned();
    let out = text(read("out.txt"));
    let err = text(read("err.txt"));
    print!("{out}");
    eprint!("{err}");
    let code = read("code.txt")
        .ok_or_else(|| format!("the elevated command did not run: {}", err.trim()))?;
    String::from_utf8_lossy(&code)
        .trim()
        .parse::<i32>()
        .map_err(|_| "the elevated command reported no exit code".to_string())
}

fn run_sudo(exe: &Path, args: &[String]) -> Result<i32, String> {
    match std::process::Command::new("sudo")
        .arg(exe)
        .args(args)
        .status()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("sudo is not installed: run the command as root".into())
        }
        Err(e) => Err(format!("cannot start sudo: {e}")),
        Ok(status) => status
            .code()
            .ok_or_else(|| "the elevated command was stopped by a signal".into()),
    }
}
