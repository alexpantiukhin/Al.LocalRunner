//! `localrunnerw`: starts `localrunner` next to it with the same arguments and a hidden console,
//! without ever showing a window (spec 10: autostart of a hub or runner by Task Scheduler);
//! its exit code is `localrunner`'s, so the task's restart on failure sees a failure.
#![windows_subsystem = "windows"]

fn main() {
    std::process::exit(run());
}

#[cfg(windows)]
fn run() -> i32 {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(me) = std::env::current_exe() else {
        return 2;
    };
    std::process::Command::new(me.with_file_name("localrunner.exe"))
        .args(std::env::args_os().skip(1))
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(2)
}

#[cfg(not(windows))]
fn run() -> i32 {
    2
}
