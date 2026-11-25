use std::io::IsTerminal;

pub(super) fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

#[cfg(windows)]
pub(super) fn enable_color() -> bool {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
            || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
pub(super) fn enable_color() -> bool {
    true
}

#[cfg(windows)]
fn terminal_width() -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let columns = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        usize::try_from(columns).ok().filter(|c| *c > 0)
    }
}

#[cfg(unix)]
fn terminal_width() -> Option<usize> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let done = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (done == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

pub(super) fn width() -> Option<usize> {
    terminal_width().or_else(|| {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.trim().parse::<usize>().ok())
            .filter(|c| *c > 0)
    })
}
