//! The `journal` sink (spec 12.1, optional, OS dependent): syslog on Unix (journald collects
//! it on systemd hosts), the Windows Event Log (Application, source [`EVENT_SOURCE`]) on
//! Windows. Lines are text without time; the OS stamps entries.

use crate::format::text_line;
use crate::queue::Writer;
use crate::redact::{Redacted, TRUNCATED};
use lr_abstractions::Level;
use std::io;
use std::sync::{Arc, Mutex};

/// Event source name in the Windows Event Log, ident in syslog.
pub const EVENT_SOURCE: &str = "Al.LocalRunner";

/// Longest insertion string `ReportEventW` accepts, in UTF-16 units.
pub const EVENT_TEXT_MAX: usize = 31_839;

/// The text cut to [`EVENT_TEXT_MAX`] UTF-16 units (at a character boundary, ending in
/// [`TRUNCATED`] when cut), so an over-long record is shortened instead of refused.
pub fn event_text(text: &str) -> String {
    if text.encode_utf16().count() <= EVENT_TEXT_MAX {
        return text.to_string();
    }
    let budget = EVENT_TEXT_MAX - TRUNCATED.encode_utf16().count();
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        used += c.len_utf16();
        if used > budget {
            break;
        }
        out.push(c);
    }
    out.push_str(TRUNCATED);
    out
}

/// The OS journal API.
pub trait OsLogApi: Send + Sync {
    /// Writes one entry.
    fn emit(&self, level: Level, text: &str) -> io::Result<()>;
}

/// syslog priority of a level (`LOG_ERR` 3, `LOG_WARNING` 4, `LOG_INFO` 6, `LOG_DEBUG` 7).
pub fn syslog_priority(level: Level) -> i32 {
    match level {
        Level::Error => 3,
        Level::Warn => 4,
        Level::Info => 6,
        Level::Debug | Level::Trace => 7,
    }
}

/// Windows event type of a level (error 1, warning 2, information 4).
pub fn event_type(level: Level) -> u16 {
    match level {
        Level::Error => 1,
        Level::Warn => 2,
        _ => 4,
    }
}

/// The journal of this OS, `None` where there is none.
pub fn system() -> Option<Arc<dyn OsLogApi>> {
    #[cfg(windows)]
    {
        windows::EventLog::open().map(|l| Arc::new(l) as Arc<dyn OsLogApi>)
    }
    #[cfg(unix)]
    {
        Some(Arc::new(unix::Syslog::open()))
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// Writes text lines to an [`OsLogApi`].
pub struct OsLogWriter {
    api: Arc<dyn OsLogApi>,
    errors: u64,
}

impl OsLogWriter {
    /// A writer to `api`.
    pub fn new(api: Arc<dyn OsLogApi>) -> OsLogWriter {
        OsLogWriter { api, errors: 0 }
    }
}

impl Writer for OsLogWriter {
    fn write(&mut self, batch: &[Redacted]) -> u64 {
        let mut lost = 0u64;
        for record in batch {
            let line = text_line(record, false);
            if self
                .api
                .emit(
                    record.record().level,
                    &event_text(line.trim_end_matches('\n')),
                )
                .is_err()
            {
                lost = lost.saturating_add(1);
                self.errors = self.errors.saturating_add(1);
            }
        }
        lost
    }

    fn errors(&self) -> u64 {
        self.errors
    }
}

/// A journal in memory for tests.
#[derive(Default)]
pub struct MemoryOsLog {
    entries: Mutex<Vec<(Level, String)>>,
}

impl MemoryOsLog {
    /// No entries.
    pub fn new() -> MemoryOsLog {
        MemoryOsLog::default()
    }

    /// Entries written so far.
    pub fn entries(&self) -> Vec<(Level, String)> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Texts written so far.
    pub fn lines(&self) -> Vec<String> {
        self.entries().into_iter().map(|(_, t)| t).collect()
    }
}

impl OsLogApi for MemoryOsLog {
    fn emit(&self, level: Level, text: &str) -> io::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((level, text.to_string()));
        Ok(())
    }
}

#[cfg(windows)]
mod windows {
    use super::{event_type, OsLogApi, EVENT_SOURCE};
    use lr_abstractions::Level;
    use std::io;
    use windows_sys::Win32::System::EventLog::{
        DeregisterEventSource, RegisterEventSourceW, ReportEventW,
    };

    pub(super) struct EventLog(*mut core::ffi::c_void);

    unsafe impl Send for EventLog {}
    unsafe impl Sync for EventLog {}

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16()
            .filter(|c| *c != 0)
            .chain(Some(0))
            .collect()
    }

    impl EventLog {
        pub(super) fn open() -> Option<EventLog> {
            let name = wide(EVENT_SOURCE);
            let handle = unsafe { RegisterEventSourceW(std::ptr::null(), name.as_ptr()) };
            (!handle.is_null()).then_some(EventLog(handle))
        }
    }

    impl OsLogApi for EventLog {
        fn emit(&self, level: Level, text: &str) -> io::Result<()> {
            let text = wide(text);
            let strings = [text.as_ptr()];
            let ok = unsafe {
                ReportEventW(
                    self.0,
                    event_type(level),
                    0,
                    0,
                    std::ptr::null_mut(),
                    1,
                    0,
                    strings.as_ptr(),
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    impl Drop for EventLog {
        fn drop(&mut self) {
            unsafe { DeregisterEventSource(self.0) };
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::{syslog_priority, OsLogApi};
    use lr_abstractions::Level;
    use std::ffi::CString;
    use std::io;

    pub(super) struct Syslog;

    impl Syslog {
        pub(super) fn open() -> Syslog {
            unsafe { libc::openlog(c"Al.LocalRunner".as_ptr(), libc::LOG_PID, libc::LOG_DAEMON) };
            Syslog
        }
    }

    impl OsLogApi for Syslog {
        fn emit(&self, level: Level, text: &str) -> io::Result<()> {
            let text = CString::new(text.replace('\0', "")).map_err(io::Error::other)?;
            unsafe { libc::syslog(syslog_priority(level), c"%s".as_ptr(), text.as_ptr()) };
            Ok(())
        }
    }
}
