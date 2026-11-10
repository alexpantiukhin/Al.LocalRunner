use std::io;

/// Gives a process without a console (a Windows service, a role started by the OS) a console
/// with no window, so the steps it starts share it and the soft stop (Ctrl+Break to the step's
/// process group) reaches them instead of ending as
/// [`crate::SOFT_STOP_NOT_DELIVERED`] (spec 10). Returns whether a console was created; a
/// process that already has one, and every other OS, needs nothing.
pub fn ensure_console() -> io::Result<bool> {
    imp::ensure()
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn ensure() -> std::io::Result<bool> {
        Ok(false)
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetConsoleProcessList, GetStdHandle, SetStdHandle, STD_ERROR_HANDLE,
        STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    const ATTACH_LIMIT: Duration = Duration::from_secs(5);
    const STD: [STD_HANDLE; 3] = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];

    fn has_console() -> bool {
        let mut pids = [0u32; 1];
        unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 1) != 0 }
    }

    pub(super) fn ensure() -> io::Result<bool> {
        if has_console() {
            return Ok(false);
        }
        let saved: Vec<HANDLE> = STD.iter().map(|h| unsafe { GetStdHandle(*h) }).collect();
        let comspec = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut host = Command::new(comspec)
            .args(["/D", "/Q", "/K"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let until = Instant::now() + ATTACH_LIMIT;
        let attached = loop {
            if unsafe { AttachConsole(host.id()) } != 0 {
                break Ok(());
            }
            let err = io::Error::last_os_error();
            if Instant::now() >= until || matches!(host.try_wait(), Ok(Some(_))) {
                break Err(err);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = host.kill();
        let _ = host.wait();
        attached?;
        for (which, handle) in STD.iter().zip(saved) {
            if !handle.is_null() {
                unsafe {
                    SetStdHandle(*which, handle);
                }
            }
        }
        Ok(true)
    }
}
