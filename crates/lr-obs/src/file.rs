//! The `file` sink (spec 12.1): JSON lines in the state directory with rotation.
//!
//! Policy: before a line would push the active file past `rotate_mb`, and once the active
//! file has been open for [`ROTATE_AGE_MS`], it is renamed to
//! `<stem>-YYYYMMDD-HHMMSS-mmm[-n]<ext>` (UTC rotation time, no characters Windows refuses)
//! and a new file is started. After each rotation and when the file is opened, rotated files
//! older than `keep_days` (by the time in their name) are deleted, then the oldest beyond
//! `keep_files`. Only names of exactly that form are ever deleted. The path comes from the
//! configuration only, never from a record.
//!
//! I/O failures never panic: they are counted in [`Writer::errors`] and the record is lost
//! (and counted as dropped); the file is reopened on the next record. A refused rename (for
//! example a reader holding the file on Windows) keeps writing to the active file and retries
//! after [`RENAME_RETRY_MS`].

use crate::format::{json_line, text_line, Format};
use crate::fs::LogFs;
use crate::queue::Writer;
use crate::redact::Redacted;
use crate::time::{civil, from_civil, Civil};
use lr_abstractions::Clock;
use lr_core::Timestamp;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

/// The active file is rotated after it has been open this long (one day).
pub const ROTATE_AGE_MS: u64 = 86_400_000;

/// Delay before a failed rotation is tried again.
pub const RENAME_RETRY_MS: u64 = 60_000;

const DAY_MS: u64 = 86_400_000;

/// Resolved settings of one file sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePolicy {
    /// Full path of the active file.
    pub path: PathBuf,
    /// Line format.
    pub format: Format,
    /// Size limit of the active file in bytes.
    pub rotate_bytes: u64,
    /// Age in days after which a rotated file is deleted.
    pub keep_days: u32,
    /// Most rotated files kept.
    pub keep_files: u32,
}

/// Name of a rotated file: `<stem>-YYYYMMDD-HHMMSS-mmm<ext>`, with `-n` before the extension
/// when `n > 0`.
pub fn rotated_name(stem: &str, ext: &str, at: Timestamp, n: u32) -> String {
    let c = civil(at);
    let suffix = if n > 0 {
        format!("-{n}")
    } else {
        String::new()
    };
    format!(
        "{stem}-{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}{suffix}{ext}",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
    )
}

fn digits(s: &str, len: usize) -> Option<u32> {
    (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn parse_rotated(stem: &str, ext: &str, name: &str) -> Option<(Timestamp, u32)> {
    let middle = name
        .strip_prefix(stem)?
        .strip_prefix('-')?
        .strip_suffix(ext)?;
    let parts: Vec<&str> = middle.split('-').collect();
    let n = match parts.len() {
        3 => 0,
        4 => {
            let p = parts[3];
            if p.is_empty() || p.starts_with('0') || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()?
        }
        _ => return None,
    };
    let date = digits(parts[0], 8)?;
    let time = digits(parts[1], 6)?;
    let at = from_civil(&Civil {
        year: i64::from(date / 10_000),
        month: date / 100 % 100,
        day: date % 100,
        hour: time / 10_000,
        minute: time / 100 % 100,
        second: time % 100,
        millis: digits(parts[2], 3)?,
    })?;
    Some((at, n))
}

/// Writes lines to the active file and rotates it by the [`FilePolicy`].
pub struct FileWriter {
    policy: FilePolicy,
    fs: Arc<dyn LogFs>,
    clock: Arc<dyn Clock>,
    dir: PathBuf,
    stem: String,
    ext: String,
    file: Option<Box<dyn Write + Send>>,
    len: u64,
    opened_at: Timestamp,
    retry_at: Option<Timestamp>,
    errors: u64,
}

impl FileWriter {
    /// A writer for `policy`; nothing is opened before the first record.
    pub fn new(policy: FilePolicy, fs: Arc<dyn LogFs>, clock: Arc<dyn Clock>) -> FileWriter {
        let dir = policy.path.parent().map(PathBuf::from).unwrap_or_default();
        let stem = policy
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("log")
            .to_string();
        let ext = policy
            .path
            .extension()
            .and_then(|s| s.to_str())
            .map_or(String::new(), |e| format!(".{e}"));
        FileWriter {
            policy,
            fs,
            clock,
            dir,
            stem,
            ext,
            file: None,
            len: 0,
            opened_at: Timestamp(0),
            retry_at: None,
            errors: 0,
        }
    }

    fn fail(&mut self) {
        self.errors = self.errors.saturating_add(1);
    }

    fn open(&mut self, now: Timestamp) -> bool {
        if self.fs.create_dir_all(&self.dir).is_err() {
            self.fail();
            return false;
        }
        match self.fs.open_append(&self.policy.path) {
            Ok((file, len)) => {
                self.file = Some(file);
                self.len = len;
                self.opened_at = now;
                self.prune(now);
                true
            }
            Err(_) => {
                self.fail();
                false
            }
        }
    }

    fn rotation_due(&self, now: Timestamp, line: u64) -> bool {
        let full = self.len.saturating_add(line) > self.policy.rotate_bytes;
        let old = now.millis_since(self.opened_at) >= ROTATE_AGE_MS;
        self.len > 0 && (full || old) && self.retry_at.is_none_or(|t| now >= t)
    }

    fn rotate(&mut self, now: Timestamp) {
        if let Some(mut file) = self.file.take() {
            if file.flush().is_err() {
                self.fail();
            }
        }
        let names = match self.fs.list(&self.dir) {
            Ok(names) => names,
            Err(_) => {
                self.fail();
                self.retry_at = Some(Timestamp(now.0.saturating_add(RENAME_RETRY_MS)));
                return;
            }
        };
        let target = (0u32..)
            .map(|n| rotated_name(&self.stem, &self.ext, now, n))
            .find(|name| !names.contains(name))
            .unwrap_or_default();
        match self.fs.rename(&self.policy.path, &self.dir.join(target)) {
            Ok(()) => {
                self.retry_at = None;
                self.prune(now);
            }
            Err(_) => {
                self.fail();
                self.retry_at = Some(Timestamp(now.0.saturating_add(RENAME_RETRY_MS)));
            }
        }
    }

    fn prune(&mut self, now: Timestamp) {
        let names = match self.fs.list(&self.dir) {
            Ok(names) => names,
            Err(_) => {
                self.fail();
                return;
            }
        };
        let mut rotated: Vec<(Timestamp, u32, String)> = names
            .into_iter()
            .filter_map(|name| {
                parse_rotated(&self.stem, &self.ext, &name).map(|(at, n)| (at, n, name))
            })
            .collect();
        rotated.sort();
        let max_age = u64::from(self.policy.keep_days).saturating_mul(DAY_MS);
        let (expired, mut kept): (Vec<_>, Vec<_>) = rotated
            .into_iter()
            .partition(|(at, _, _)| now.millis_since(*at) > max_age);
        let extra = kept.len().saturating_sub(self.policy.keep_files as usize);
        let surplus: Vec<_> = kept.drain(..extra).collect();
        for (_, _, name) in expired.into_iter().chain(surplus) {
            if self.fs.remove_file(&self.dir.join(name)).is_err() {
                self.fail();
            }
        }
    }

    fn write_line(&mut self, line: &[u8]) -> bool {
        let now = self.clock.now();
        if self.file.is_none() && !self.open(now) {
            return false;
        }
        if self.rotation_due(now, line.len() as u64) {
            self.rotate(now);
            if self.file.is_none() && !self.open(now) {
                return false;
            }
        }
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        match file.write_all(line) {
            Ok(()) => {
                self.len = self.len.saturating_add(line.len() as u64);
                true
            }
            Err(_) => {
                self.file = None;
                self.fail();
                false
            }
        }
    }
}

impl Writer for FileWriter {
    fn write(&mut self, batch: &[Redacted]) -> u64 {
        let mut lost = 0u64;
        for record in batch {
            let line = match self.policy.format {
                Format::Json => json_line(record),
                Format::Text => text_line(record, true),
            };
            if !self.write_line(line.as_bytes()) {
                lost = lost.saturating_add(1);
            }
        }
        lost
    }

    fn flush(&mut self) -> u64 {
        if let Some(file) = self.file.as_mut() {
            if file.flush().is_err() {
                self.file = None;
                self.fail();
            }
        }
        0
    }

    fn errors(&self) -> u64 {
        self.errors
    }
}
