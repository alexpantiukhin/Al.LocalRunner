use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Longest line written to a log; longer lines are split with an inserted newline.
pub const MAX_LINE: usize = 64 * 1024;

/// The header line that starts the output of a step in a job log (spec 6, 7.1):
/// `=== step <name> | <field>=<value> | ... ===`, with the job fields (project, source, label,
/// note, branch, cwd, cancel reason) in the given order. Line breaks in the name and the
/// fields are replaced by spaces, so neither can forge a second header line.
pub fn header_line(step: &str, fields: &[(String, String)]) -> String {
    let mut line = format!("=== step {}", one_line(step));
    for (name, value) in fields {
        line.push_str(&format!(" | {}={}", one_line(name), one_line(value)));
    }
    line.push_str(" ===\n");
    line
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Appends raw job output to a log file with a bounded line buffer.
///
/// Bytes are never decoded. A line is split only when more bytes follow `MAX_LINE`, so
/// exactly `MAX_LINE` bytes then a newline stay one line, and the writer never holds more
/// than `MAX_LINE` bytes. Dropping the writer terminates an unfinished trailing line, so a
/// later append cannot glue its first line to it.
pub struct LogWriter {
    out: BufWriter<File>,
    line: Vec<u8>,
    dangling: bool,
}

impl LogWriter {
    /// Opens the log for appending, creating it when missing.
    pub fn create(path: &Path) -> io::Result<LogWriter> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogWriter {
            out: BufWriter::new(file),
            line: Vec::with_capacity(MAX_LINE),
            dangling: false,
        })
    }

    /// Appends output bytes; a line longer than `MAX_LINE` is split.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while let Some(&first) = rest.first() {
            if self.line.len() == MAX_LINE && first != b'\n' {
                self.emit_line(true)?;
            }
            let room = (MAX_LINE - self.line.len()).max(1);
            let window = &rest[..rest.len().min(room)];
            match window.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    self.line.extend_from_slice(&window[..=i]);
                    self.emit_line(false)?;
                    rest = &rest[i + 1..];
                }
                None => {
                    self.line.extend_from_slice(window);
                    rest = &rest[window.len()..];
                }
            }
        }
        if let Some(&last) = bytes.last() {
            self.dangling = last != b'\n';
        }
        Ok(())
    }

    /// Number of bytes held back in the line buffer.
    pub fn buffered(&self) -> usize {
        self.line.len()
    }

    /// Writes the pending partial line and flushes the file.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.write_all(&self.line)?;
        self.line.clear();
        self.out.flush()
    }

    fn emit_line(&mut self, newline: bool) -> io::Result<()> {
        self.out.write_all(&self.line)?;
        self.line.clear();
        if newline {
            self.out.write_all(b"\n")?;
        }
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        let _ = self.flush();
        if self.dangling {
            let _ = self.out.write_all(b"\n");
            let _ = self.out.flush();
        }
    }
}
