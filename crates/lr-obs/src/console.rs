//! The `console` sink: text lines on stderr, only while the process runs in the foreground
//! (the caller passes a console writer only then).

use crate::format::text_line;
use crate::logger::SharedWriter;
use crate::queue::Writer;
use crate::redact::Redacted;

/// Writes text lines to a shared writer (stderr in production).
pub struct ConsoleWriter {
    out: SharedWriter,
    errors: u64,
}

impl ConsoleWriter {
    /// A writer to `out`.
    pub fn new(out: SharedWriter) -> ConsoleWriter {
        ConsoleWriter { out, errors: 0 }
    }
}

impl Writer for ConsoleWriter {
    fn write(&mut self, batch: &[Redacted]) -> u64 {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let mut lost = 0u64;
        for record in batch {
            if out.write_all(text_line(record, true).as_bytes()).is_err() {
                lost = lost.saturating_add(1);
                self.errors = self.errors.saturating_add(1);
            }
        }
        lost
    }

    fn flush(&mut self) -> u64 {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        if out.flush().is_err() {
            self.errors = self.errors.saturating_add(1);
        }
        0
    }

    fn errors(&self) -> u64 {
        self.errors
    }
}
