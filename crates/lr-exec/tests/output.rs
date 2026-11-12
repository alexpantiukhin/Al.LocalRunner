mod common;

use common::{events_bytes, fresh_file, helper_command, job, step, Helper, Recorder};
use lr_abstractions::{stop_signal, JobEvent, JobSpec, ProcessRunner, SecretValue};
use lr_core::JobState;
use lr_exec::{ExecRunner, LogWriter, Masker, SystemClock, MAX_CHUNK, MAX_LINE};
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
const BOTH_STREAMS: &str = "echo out1&echo out2&echo err1 1>&2&echo err2 1>&2";
#[cfg(unix)]
const BOTH_STREAMS: &str = "echo out1; echo out2; echo err1 >&2; echo err2 >&2";

async fn run(spec: JobSpec) -> (JobState, Arc<Recorder>) {
    let runner = ExecRunner::new(Arc::new(SystemClock));
    let out = Recorder::new();
    let (_handle, stop) = stop_signal();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run_job(spec, out.as_ref(), stop),
    )
    .await
    .expect("run_job did not return");
    (outcome.state, out)
}

fn secret(value: &str) -> SecretValue {
    SecretValue::new(value)
}

fn feed(masker: &mut Masker, pieces: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for piece in pieces {
        out.extend(masker.push(piece));
    }
    out.extend(masker.finish());
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_utf8_and_10mb_line_stay_bounded() {
    let cmd = helper_command(&Helper::new("big"));
    let (state, out) = run(job("big", 60, vec![step("big", &cmd, 50)])).await;
    assert_eq!(state, JobState::Passed);
    assert!(out.max_chunk() <= MAX_CHUNK, "chunk of {}", out.max_chunk());
    let bytes = events_bytes(&out.events());
    let at = bytes
        .windows(2)
        .position(|w| w == [0xff, 0xfe])
        .expect("raw bytes lost");
    let total = 10 * 1024 * 1024;
    assert!(bytes[at + 2..at + 2 + total].iter().all(|b| *b == b'a'));

    let path = fresh_file("output", "big.log");
    let mut log = LogWriter::create(&path).unwrap();
    for event in out.events() {
        if let JobEvent::Output { data, .. } = event {
            log.write(&data).unwrap();
            assert!(log.buffered() <= MAX_LINE);
        }
    }
    log.flush().unwrap();
    drop(log);
    let logged = std::fs::read(&path).unwrap();
    assert!(logged.split(|b| *b == b'\n').all(|l| l.len() <= MAX_LINE));
    assert!(logged.windows(3).any(|w| w == [0xff, 0xfe, b'a']));
    assert!(logged.iter().filter(|b| **b == b'a').count() >= total);
}

#[tokio::test]
async fn secret_split_across_output_chunks_is_masked() {
    let cmd = helper_command(&Helper::new("split"));
    let mut spec = job("split", 60, vec![step("s", &cmd, 30)]);
    spec.steps[0].secrets = vec![("X".to_string(), secret("SECRET"))];
    let (state, out) = run(spec).await;
    assert_eq!(state, JobState::Passed);
    let text = out.text();
    assert!(text.contains("pre *** post"), "{text}");
    assert!(!text.contains("SECRET"));
    let pieces = out
        .events()
        .iter()
        .filter(|e| matches!(e, JobEvent::Output { .. }))
        .count();
    assert!(pieces >= 2);
}

#[tokio::test]
async fn stdout_and_stderr_are_both_captured_in_order_per_stream() {
    let (state, out) = run(job("both", 60, vec![step("s", BOTH_STREAMS, 30)])).await;
    assert_eq!(state, JobState::Passed);
    let text = out.text();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let pos = |needle: &str| {
        lines
            .iter()
            .position(|l| *l == needle)
            .unwrap_or_else(|| panic!("{needle} missing in {lines:?}"))
    };
    assert!(pos("out1") < pos("out2"));
    assert!(pos("err1") < pos("err2"));
}

#[test]
fn masker_finds_a_secret_at_every_split_point() {
    let input = b"aaSECRETbb";
    for cut in 0..=input.len() {
        let s = secret("SECRET");
        let mut masker = Masker::new(&[&s]);
        let got = feed(&mut masker, &[&input[..cut], &input[cut..]]);
        assert_eq!(got, b"aa***bb", "cut at {cut}");
    }
    let s = secret("SECRET");
    let mut masker = Masker::new(&[&s]);
    let bytes: Vec<&[u8]> = input.chunks(1).collect();
    assert_eq!(feed(&mut masker, &bytes), b"aa***bb");
}

#[test]
fn masker_holds_back_only_a_possible_prefix() {
    let s = secret("SECRET");
    let mut masker = Masker::new(&[&s]);
    assert_eq!(masker.push(b"hello SEC"), b"hello ");
    assert_eq!(masker.held(), 3);
    assert_eq!(masker.push(b"OND"), b"SECOND");
    assert_eq!(masker.held(), 0);
    assert_eq!(masker.push(b"SECSECRET!"), b"SEC***!");
    assert_eq!(masker.push(b"SECR"), b"");
    assert_eq!(masker.finish(), b"SECR");
}

#[test]
fn masker_held_bytes_stay_below_the_longest_secret() {
    let s = secret("abcdefgh");
    let mut masker = Masker::new(&[&s]);
    let data = b"abcdefgabcdefgabcdefg".repeat(1000);
    for piece in data.chunks(7) {
        masker.push(piece);
        assert!(masker.held() < 8);
    }
}

#[test]
fn masker_prefers_the_longest_secret_and_ignores_empty_ones() {
    let short = secret("abc");
    let long = secret("abcdef");
    let empty = secret("");
    let mut masker = Masker::new(&[&short, &long, &empty]);
    assert_eq!(feed(&mut masker, &[b"xabc", b"defy xabcy"]), b"x***y x***y");
    let mut none = Masker::new(&[&empty]);
    assert_eq!(feed(&mut none, &[b"plain", b" text"]), b"plain text");
}

#[test]
fn masker_passes_non_utf8_bytes_unchanged() {
    let s = secret("tok");
    let mut masker = Masker::new(&[&s]);
    let input = [0xff, 0xfe, b't', b'o', b'k', 0x80];
    assert_eq!(
        feed(&mut masker, &[&input]),
        [0xff, 0xfe, b'*', b'*', b'*', 0x80]
    );
}
