use lr_abstractions::fakes::ManualClock;
use lr_abstractions::{Clock, Level, Record, Role};
use lr_core::Timestamp;
use lr_obs::file::{rotated_name, FilePolicy, FileWriter, RENAME_RETRY_MS, ROTATE_AGE_MS};
use lr_obs::format::{json_line, Format};
use lr_obs::fs::{LogFs, MemoryLogFs, OsLogFs};
use lr_obs::queue::Writer;
use lr_obs::redact::{Redacted, Redactor};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const T0: Timestamp = Timestamp(1_700_000_000_000);
const DAY_MS: u64 = 86_400_000;

fn dir() -> PathBuf {
    PathBuf::from("state").join("logs")
}

fn policy(rotate_mb: u64, keep_days: u32, keep_files: u32) -> FilePolicy {
    FilePolicy {
        path: dir().join("daemon.log"),
        format: Format::Json,
        rotate_bytes: rotate_mb * 1024 * 1024,
        keep_days,
        keep_files,
    }
}

struct Bench {
    clock: ManualClock,
    fs: MemoryLogFs,
    writer: FileWriter,
}

fn bench(policy: FilePolicy) -> Bench {
    let clock = ManualClock::new(T0);
    let fs = MemoryLogFs::new();
    let shared_clock: Arc<dyn Clock> = Arc::new(clock.clone());
    let shared_fs: Arc<dyn LogFs> = Arc::new(fs.clone());
    let writer = FileWriter::new(policy, shared_fs, shared_clock);
    Bench { clock, fs, writer }
}

fn redacted(message: &str, at: Timestamp) -> Redacted {
    let record = Record::new(at, Level::Info, Role::Broker, "engine", message);
    Redactor::new(&[]).redact(&record)
}

fn sized(len: usize, at: Timestamp) -> Redacted {
    let overhead = json_line(&redacted("", at)).len();
    let r = redacted(&"x".repeat(len - overhead), at);
    assert_eq!(json_line(&r).len(), len);
    r
}

fn lines(fs: &MemoryLogFs, path: &Path) -> usize {
    fs.file(path)
        .map_or(0, |d| d.iter().filter(|b| **b == b'\n').count())
}

#[test]
fn file_sink_rotates_by_policy() {
    let mut b = bench(policy(1, 14, 10));
    let line = sized(1024, T0);
    for _ in 0..1024 {
        assert_eq!(b.writer.write(std::slice::from_ref(&line)), 0);
    }
    assert_eq!(b.fs.names(&dir()), vec!["daemon.log"]);
    assert_eq!(
        b.fs.file(&dir().join("daemon.log")).unwrap().len(),
        1_048_576
    );
    b.clock.advance(Duration::from_millis(1234));
    b.writer.write(std::slice::from_ref(&line));
    let rotated = "daemon-20231114-221321-234.log";
    assert_eq!(b.fs.names(&dir()), vec![rotated, "daemon.log"]);
    assert_eq!(lines(&b.fs, &dir().join(rotated)), 1024);
    assert_eq!(lines(&b.fs, &dir().join("daemon.log")), 1);
}

#[test]
fn default_rotation_threshold_is_fifty_mebibytes() {
    assert_eq!(policy(50, 14, 10).rotate_bytes, 52_428_800);
}

#[test]
fn active_file_rotates_after_one_day() {
    assert_eq!(ROTATE_AGE_MS, DAY_MS);
    let mut b = bench(policy(50, 14, 10));
    b.writer.write(&[redacted("first", T0)]);
    b.clock.advance(Duration::from_millis(DAY_MS - 1));
    b.writer.write(&[redacted("same file", T0)]);
    assert_eq!(b.fs.names(&dir()), vec!["daemon.log"]);
    b.clock.advance(Duration::from_millis(1));
    b.writer.write(&[redacted("next file", T0)]);
    assert_eq!(
        b.fs.names(&dir()),
        vec!["daemon-20231115-221320-000.log", "daemon.log"]
    );
}

#[test]
fn rotated_names_are_safe_and_unique() {
    assert_eq!(
        rotated_name("daemon", ".log", T0, 0),
        "daemon-20231114-221320-000.log"
    );
    assert_eq!(
        rotated_name("daemon", ".log", T0, 2),
        "daemon-20231114-221320-000-2.log"
    );
    assert_eq!(
        rotated_name("trace", "", T0, 0),
        "trace-20231114-221320-000"
    );
    let name = rotated_name("daemon", ".log", T0, 1);
    assert!(!name.contains([':', '<', '>', '"', '|', '?', '*', '\\', '/']));
}

#[test]
fn rotation_in_the_same_millisecond_gets_a_suffix() {
    let mut p = policy(1, 14, 10);
    p.rotate_bytes = 10_000;
    let mut b = bench(p);
    let big = sized(7_000, T0);
    for _ in 0..3 {
        b.writer.write(std::slice::from_ref(&big));
    }
    assert_eq!(
        b.fs.names(&dir()),
        vec![
            "daemon-20231114-221320-000-1.log",
            "daemon-20231114-221320-000.log",
            "daemon.log"
        ]
    );
}

#[test]
fn rotated_files_older_than_keep_days_are_removed() {
    let mut b = bench(policy(50, 14, 10));
    let now = Timestamp(T0.0 + 20 * DAY_MS);
    b.clock.advance(Duration::from_millis(20 * DAY_MS));
    let expired = rotated_name("daemon", ".log", Timestamp(now.0 - 14 * DAY_MS - 1), 0);
    let kept = rotated_name("daemon", ".log", Timestamp(now.0 - 14 * DAY_MS), 0);
    b.fs.put(&dir().join(&expired), b"old\n");
    b.fs.put(&dir().join(&kept), b"recent\n");
    b.writer.write(&[redacted("opens and prunes", now)]);
    assert_eq!(b.fs.names(&dir()), vec![kept, "daemon.log".to_string()]);
}

#[test]
fn at_most_keep_files_rotated_files_are_kept() {
    let mut b = bench(policy(50, 14, 3));
    let names: Vec<String> = (0..5)
        .map(|n| rotated_name("daemon", ".log", Timestamp(T0.0 - 1000 * (5 - n)), 0))
        .collect();
    for name in &names {
        b.fs.put(&dir().join(name), b"x\n");
    }
    b.writer.write(&[redacted("m", T0)]);
    let mut expected = names[2..].to_vec();
    expected.push("daemon.log".to_string());
    assert_eq!(b.fs.names(&dir()), expected);
}

#[test]
fn unrelated_files_in_the_log_directory_are_never_removed() {
    let mut b = bench(policy(50, 1, 1));
    b.clock.advance(Duration::from_millis(30 * DAY_MS));
    for name in [
        "other-20231114-221320-000.log",
        "daemon-2023111-221320-000.log",
        "daemon-20231114-221320-000.txt",
        "daemon-20231114-221320-000-x.log",
        "notes.txt",
    ] {
        b.fs.put(&dir().join(name), b"keep\n");
    }
    b.writer.write(&[redacted("m", T0)]);
    assert_eq!(b.fs.names(&dir()).len(), 6);
}

#[test]
fn io_errors_never_panic_are_counted_and_recover() {
    let mut b = bench(policy(50, 14, 10));
    b.fs.fail(Some("disk full"));
    assert_eq!(b.writer.write(&[redacted("a", T0), redacted("b", T0)]), 2);
    assert_eq!(b.writer.errors(), 2);
    b.fs.fail(None);
    assert_eq!(b.writer.write(&[redacted("c", T0)]), 0);
    assert_eq!(lines(&b.fs, &dir().join("daemon.log")), 1);
}

#[test]
fn rename_failure_keeps_writing_and_retries_later() {
    assert_eq!(RENAME_RETRY_MS, 60_000);
    let mut p = policy(1, 14, 10);
    p.rotate_bytes = 10_000;
    let mut b = bench(p);
    let big = sized(7_000, T0);
    b.writer.write(std::slice::from_ref(&big));
    b.fs.fail_rename(true);
    assert_eq!(b.writer.write(std::slice::from_ref(&big)), 0);
    assert_eq!(b.writer.errors(), 1);
    assert_eq!(b.fs.names(&dir()), vec!["daemon.log"]);
    b.fs.fail_rename(false);
    b.clock.advance(Duration::from_millis(RENAME_RETRY_MS - 1));
    b.writer.write(&[redacted("still waiting", T0)]);
    assert_eq!(b.fs.names(&dir()), vec!["daemon.log"]);
    b.clock.advance(Duration::from_millis(1));
    b.writer.write(&[redacted("rotates now", T0)]);
    assert_eq!(b.fs.names(&dir()).len(), 2);
    assert_eq!(lines(&b.fs, &dir().join("daemon.log")), 1);
}

#[test]
fn appends_to_an_existing_active_file() {
    let mut b = bench(policy(50, 14, 10));
    b.fs.put(&dir().join("daemon.log"), b"previous run\n");
    b.writer.write(&[redacted("new run", T0)]);
    assert_eq!(lines(&b.fs, &dir().join("daemon.log")), 2);
}

#[test]
fn json_lines_carry_spec_fields_on_one_line() {
    let record = Record {
        time: Timestamp(1_700_000_000_123),
        level: Level::Warn,
        role: Role::Hub,
        component: "outbox".into(),
        machine_id: "m-1".into(),
        message: "line1\nline2\u{1b}[31m".into(),
        fields: vec![
            ("job_id".into(), "j1".into()),
            ("job_id".into(), "j2".into()),
        ],
    };
    let line = json_line(&Redactor::new(&[]).redact(&record));
    assert_eq!(
        line,
        concat!(
            r#"{"time":"2023-11-14T22:13:20.123Z","level":"warn","role":"hub","#,
            r#""component":"outbox","machine_id":"m-1","#,
            r#""message":"line1\nline2\u001b[31m","fields":{"job_id":"j2"}}"#,
            "\n"
        )
    );
}

#[test]
fn flush_writes_buffered_lines_through() {
    let mut b = bench(policy(50, 14, 10));
    b.writer.write(&[redacted("m", T0)]);
    b.writer.flush();
    assert!(b.fs.flushes() >= 1);
}

#[test]
fn os_file_system_appends_renames_lists_and_removes() {
    let root = std::env::temp_dir().join(format!("lr-obs-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fs = OsLogFs;
    let logs = root.join("logs");
    fs.create_dir_all(&logs).unwrap();
    let path = logs.join("daemon.log");
    let (mut file, len) = fs.open_append(&path).unwrap();
    assert_eq!(len, 0);
    file.write_all(b"one\n").unwrap();
    file.flush().unwrap();
    drop(file);
    let (mut file, len) = fs.open_append(&path).unwrap();
    assert_eq!(len, 4);
    file.write_all(b"two\n").unwrap();
    file.flush().unwrap();
    drop(file);
    fs.rename(&path, &logs.join("old.log")).unwrap();
    assert_eq!(fs.list(&logs).unwrap(), vec!["old.log".to_string()]);
    assert_eq!(std::fs::read(logs.join("old.log")).unwrap(), b"one\ntwo\n");
    fs.remove_file(&logs.join("old.log")).unwrap();
    assert!(fs.list(&logs).unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let (_, _) = fs.open_append(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn text_format_files_carry_time_and_escape_controls() {
    let mut p = policy(50, 14, 10);
    p.format = Format::Text;
    let mut b = bench(p);
    b.writer.write(&[redacted("one\ntwo", T0)]);
    let text = String::from_utf8(b.fs.file(&dir().join("daemon.log")).unwrap()).unwrap();
    assert_eq!(
        text,
        "2023-11-14T22:13:20.000Z INFO  broker engine one\\ntwo\n"
    );
}
