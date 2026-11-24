use super::document::ConfigDoc;
use super::target::{resolve, Target};
use super::validate::validate;
use crate::paths::Layout;
use lr_abstractions::Clock;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const WIDENING_KEYS: [&str; 2] = ["allow_elevated", "allow_same_user_as_broker"];
const CONTEXT: usize = 3;

/// A planned edit of one configuration file: the text before and after, what validation warned about and which safety switches the edit turns on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file the edit writes.
    pub path: PathBuf,
    /// The text read from the file; empty when the file does not exist.
    pub before: String,
    /// The text to write.
    pub after: String,
    /// Warnings of the role's own validation of `after`.
    pub warnings: Vec<String>,
    /// Keys among `allow_elevated` and `allow_same_user_as_broker` that the edit turned to `true`.
    pub widens: Vec<String>,
}

#[derive(Clone, Copy)]
enum Op {
    Keep,
    Del,
    Add,
}

fn script<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(Op, &'a str)> {
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if old[i] == new[j] {
            out.push((Op::Keep, old[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push((Op::Del, old[i]));
            i += 1;
        } else {
            out.push((Op::Add, new[j]));
            j += 1;
        }
    }
    out.extend(old[i..].iter().map(|l| (Op::Del, *l)));
    out.extend(new[j..].iter().map(|l| (Op::Add, *l)));
    out
}

fn range(start: usize, len: usize) -> String {
    match len {
        1 => format!("{start}"),
        0 => format!("{},0", start.saturating_sub(1)),
        _ => format!("{start},{len}"),
    }
}

impl Change {
    /// True when the edit leaves the text as it was.
    pub fn is_noop(&self) -> bool {
        self.before == self.after
    }

    /// A unified diff with three lines of context; the header names the file.
    pub fn diff(&self) -> String {
        let old: Vec<&str> = self.before.split_inclusive('\n').collect();
        let new: Vec<&str> = self.after.split_inclusive('\n').collect();
        let ops = script(&old, &new);
        let changed: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, (op, _))| !matches!(op, Op::Keep))
            .map(|(i, _)| i)
            .collect();
        let path = self.path.display();
        let mut out = format!("--- {path}\n+++ {path}\n");
        let mut k = 0;
        while k < changed.len() {
            let start = changed[k].saturating_sub(CONTEXT);
            let mut end = changed[k];
            while k + 1 < changed.len() && changed[k + 1] <= end + 2 * CONTEXT + 1 {
                k += 1;
                end = changed[k];
            }
            k += 1;
            let end = (end + CONTEXT + 1).min(ops.len());
            let old_start = ops[..start]
                .iter()
                .filter(|(op, _)| !matches!(op, Op::Add))
                .count()
                + 1;
            let new_start = ops[..start]
                .iter()
                .filter(|(op, _)| !matches!(op, Op::Del))
                .count()
                + 1;
            let hunk = &ops[start..end];
            let old_len = hunk.iter().filter(|(op, _)| !matches!(op, Op::Add)).count();
            let new_len = hunk.iter().filter(|(op, _)| !matches!(op, Op::Del)).count();
            out.push_str(&format!(
                "@@ -{} +{} @@\n",
                range(old_start, old_len),
                range(new_start, new_len)
            ));
            for (op, line) in hunk {
                let mark = match op {
                    Op::Keep => ' ',
                    Op::Del => '-',
                    Op::Add => '+',
                };
                out.push(mark);
                out.push_str(line.strip_suffix('\n').unwrap_or(line));
                out.push('\n');
                if !line.ends_with('\n') {
                    out.push_str("\\ No newline at end of file\n");
                }
            }
        }
        out
    }
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn is_true(doc: &ConfigDoc, key: &str) -> bool {
    doc.get(key).as_deref() == Some("true")
}

/// Reads the file the role of `target` reads (a missing file is empty), applies `edit`, validates the result and returns the change; nothing is written.
pub fn plan(
    layout: &Layout,
    target: &Target,
    edit: impl FnOnce(&mut ConfigDoc) -> Result<(), String>,
) -> Result<Change, String> {
    let path = resolve(layout, target)?;
    let before = read_or_empty(&path)?;
    let old_doc = ConfigDoc::parse(&before)?;
    let mut doc = ConfigDoc::parse(&before)?;
    edit(&mut doc)?;
    let after = doc.render();
    let warnings = validate(target, &after, layout)?;
    let widens = WIDENING_KEYS
        .iter()
        .filter(|k| is_true(&doc, k) && !is_true(&old_doc, k))
        .map(|k| k.to_string())
        .collect();
    Ok(Change {
        path,
        before,
        after,
        warnings,
        widens,
    })
}

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_exclusive(path: &Path, mode: Option<u32>) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn keep_mode(path: &Path, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(mode) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn keep_mode(_path: &Path, _mode: Option<u32>) -> std::io::Result<()> {
    Ok(())
}

/// True when this user cannot create a file in the directory of `path`, so writing needs administrator or root rights.
pub fn needs_rights(path: &Path) -> bool {
    let Some(mut dir) = path.parent() else {
        return true;
    };
    while !dir.exists() {
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return true,
        }
    }
    let probe = dir.join(format!(".probe-{}", unique()));
    match create_exclusive(&probe, None) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

fn stamp(clock: &dyn Clock) -> String {
    let text = clock.now().to_string();
    let digits: Vec<char> = text.chars().filter(char::is_ascii_digit).collect();
    let (date, time) = digits.split_at(8);
    format!(
        "{}T{}",
        date.iter().collect::<String>(),
        time.iter().collect::<String>()
    )
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{name}{suffix}")))
}

fn back_up(path: &Path, clock: &dyn Clock, mode: Option<u32>) -> Result<PathBuf, String> {
    let stamp = stamp(clock);
    let fail = |e: std::io::Error| format!("cannot back up {}: {e}", path.display());
    let mut source = File::open(path).map_err(fail)?;
    for n in 1u32.. {
        let suffix = if n == 1 {
            format!(".bak-{stamp}")
        } else {
            format!(".bak-{stamp}-{n}")
        };
        let backup = sibling(path, &suffix)?;
        match create_exclusive(&backup, mode) {
            Ok(mut out) => {
                let copied =
                    std::io::copy(&mut source, &mut out).and_then(|_| keep_mode(&backup, mode));
                if let Err(e) = copied {
                    drop(out);
                    let removed = std::fs::remove_file(&backup);
                    return Err(backup_failure(
                        path,
                        &e.to_string(),
                        &backup,
                        removed.err().map(|r| r.to_string()).as_deref(),
                    ));
                }
                return Ok(backup);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fail(e)),
        }
    }
    unreachable!()
}

/// The error text for a backup whose copy failed: names the copy error and, when the partial backup could not be removed, that file and why.
pub fn backup_failure(
    path: &Path,
    copy_error: &str,
    partial: &Path,
    remove_error: Option<&str>,
) -> String {
    let mut message = format!("cannot back up {}: {copy_error}", path.display());
    if let Some(e) = remove_error {
        message.push_str(&format!(
            "; the partial backup {} could not be removed: {e}",
            partial.display()
        ));
    }
    message
}

const TEMP_ATTEMPTS: u64 = 100;

fn create_temp(path: &Path, first: u64, mode: Option<u32>) -> Result<(PathBuf, File), String> {
    let mut last = None;
    for n in first..first + TEMP_ATTEMPTS {
        let temp = sibling(path, &format!(".tmp-{}-{n}", std::process::id()))?;
        match create_exclusive(&temp, mode) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(format!("cannot write {}: {e}", temp.display())),
        }
    }
    Err(format!(
        "cannot create a temporary file beside {}: {}",
        path.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn fill_temp(
    temp: &Path,
    mut file: File,
    text: &str,
    original: Option<&std::fs::Permissions>,
) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", temp.display());
    file.write_all(text.as_bytes()).map_err(fail)?;
    drop(file);
    #[cfg(unix)]
    if let Some(permissions) = original {
        std::fs::set_permissions(temp, permissions.clone()).map_err(fail)?;
    }
    #[cfg(not(unix))]
    let _ = original;
    Ok(())
}

/// Writes `change.after` over the file through a unique temporary file and a rename; an existing file is copied to `<name>.bak-<UTC yyyymmddThhmmss>` first (a numeric suffix keeps an older backup of the same second) and keeps its permissions. A file whose state cannot be read is an error and stays untouched. Returns the backup path, or the file itself when there was nothing to back up.
pub fn write(change: &Change, clock: &dyn Clock) -> Result<PathBuf, String> {
    write_with_counter(
        change,
        clock,
        UNIQUE.fetch_add(TEMP_ATTEMPTS, Ordering::Relaxed),
    )
}

/// [`write`] with the first number of the temporary file name `<name>.tmp-<pid>-<n>` given by the caller; a taken name makes it try the next number.
pub fn write_with_counter(
    change: &Change,
    clock: &dyn Clock,
    first: u64,
) -> Result<PathBuf, String> {
    let path = &change.path;
    let original = match std::fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read the state of {}: {e}", path.display())),
    };
    let backup = match &original {
        Some(meta) if meta.is_file() => Some(back_up(path, clock, mode_of(meta))?),
        _ => None,
    };
    let permissions = original
        .as_ref()
        .filter(|m| m.is_file())
        .map(|m| m.permissions());
    let mode = original.as_ref().filter(|m| m.is_file()).and_then(mode_of);
    let (temp, file) = create_temp(path, first, mode)?;
    if let Err(e) = fill_temp(&temp, file, &change.after, permissions.as_ref()) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot replace {}: {e}", path.display()));
    }
    Ok(backup.unwrap_or_else(|| path.clone()))
}
