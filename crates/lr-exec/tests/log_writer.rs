use lr_exec::{header_line, LogWriter, MAX_LINE};
use std::path::{Path, PathBuf};

fn fresh_log(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("logw");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    path
}

fn patterned(len: usize, seed: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            let b = ((i * 31 + seed) % 256) as u8;
            if b == b'\n' {
                b'\r'
            } else {
                b
            }
        })
        .collect()
}

fn model(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut segments: Vec<&[u8]> = input.split(|b| *b == b'\n').collect();
    let last = segments.pop().unwrap();
    for segment in segments {
        push_segment(&mut out, segment);
        out.push(b'\n');
    }
    if !last.is_empty() {
        push_segment(&mut out, last);
    }
    out
}

fn push_segment(out: &mut Vec<u8>, segment: &[u8]) {
    for (i, piece) in segment.chunks(MAX_LINE).enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(piece);
    }
}

fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn header_text_is_pinned() {
    assert_eq!(header_line("alpha", &[]), "=== step alpha ===\n");
    assert_eq!(
        header_line(
            "alpha",
            &fields(&[
                ("project", "o/r"),
                ("label", "alex"),
                ("cancel", "bob: stale")
            ])
        ),
        "=== step alpha | project=o/r | label=alex | cancel=bob: stale ===\n"
    );
}

#[test]
fn newlines_in_name_or_fields_cannot_forge_a_header() {
    let line = header_line(
        "a ===\n=== step evil\r\nx",
        &fields(&[("note", "x\n=== step forged ===\r\n")]),
    );
    assert_eq!(line.matches('\n').count(), 1);
    assert!(!line.contains('\r'));
    assert!(line.starts_with("=== step a") && line.ends_with(" ===\n"));
}

#[test]
fn lines_around_the_boundary_are_preserved_byte_for_byte() {
    for body_len in [
        MAX_LINE - 1,
        MAX_LINE,
        MAX_LINE + 1,
        2 * MAX_LINE,
        2 * MAX_LINE + 1,
    ] {
        for feed in [1000, 7777, usize::MAX] {
            let mut input = patterned(MAX_LINE - 1, 1);
            input.push(b'\n');
            input.extend(patterned(body_len, 2));
            input.push(b'\n');
            input.extend(patterned(MAX_LINE + 1, 3));
            input.push(b'\n');
            input.extend_from_slice(b"tail\n");
            let path = fresh_log("boundary.log");
            let mut log = LogWriter::create(&path).unwrap();
            for piece in input.chunks(feed.min(input.len())) {
                log.write(piece).unwrap();
                assert!(log.buffered() <= MAX_LINE);
            }
            log.flush().unwrap();
            drop(log);
            let body = std::fs::read(&path).unwrap();
            assert!(body.split(|b| *b == b'\n').all(|l| l.len() <= MAX_LINE));
            assert_eq!(body, model(&input), "body_len={body_len} feed={feed}");
        }
    }
}

#[test]
fn exactly_max_line_then_newline_has_no_empty_line() {
    let path = fresh_log("exact.log");
    let mut log = LogWriter::create(&path).unwrap();
    let mut input = vec![b'x'; MAX_LINE];
    input.push(b'\n');
    log.write(&input).unwrap();
    log.flush().unwrap();
    drop(log);
    assert_eq!(std::fs::read(&path).unwrap(), input);
}

#[test]
fn ten_megabyte_line_without_newline_is_split_and_memory_bounded() {
    let path = fresh_log("big.log");
    let mut log = LogWriter::create(&path).unwrap();
    let chunk = vec![b'a'; 8192];
    let total = 10 * 1024 * 1024;
    let mut fed = 0;
    while fed < total {
        log.write(&chunk).unwrap();
        fed += chunk.len();
        assert!(log.buffered() <= MAX_LINE);
    }
    log.flush().unwrap();
    drop(log);
    let body = std::fs::read(&path).unwrap();
    assert!(body
        .split(|b| *b == b'\n')
        .all(|line| line.len() <= MAX_LINE));
    assert_eq!(body.iter().filter(|b| **b == b'a').count(), total);
    assert!(body.iter().filter(|b| **b == b'\n').count() >= total / MAX_LINE - 1);
}

#[test]
fn non_utf8_bytes_are_written_unchanged() {
    let path = fresh_log("raw.log");
    let mut log = LogWriter::create(&path).unwrap();
    log.write(&[0xff, 0xfe, b'A', b'\n', 0x80]).unwrap();
    drop(log);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        vec![0xff, 0xfe, b'A', b'\n', 0x80, b'\n']
    );
}

#[test]
fn dropping_terminates_a_trailing_line_so_append_cannot_glue_a_header() {
    let path = fresh_log("drop.log");
    let mut log = LogWriter::create(&path).unwrap();
    log.write(header_line("one", &[]).as_bytes()).unwrap();
    log.write(b"tail").unwrap();
    log.flush().unwrap();
    drop(log);
    let mut log = LogWriter::create(&path).unwrap();
    log.write(header_line("two", &[]).as_bytes()).unwrap();
    drop(log);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        format!(
            "{}tail\n{}",
            header_line("one", &[]),
            header_line("two", &[])
        )
    );
}

#[test]
fn cancel_line_text_is_pinned() {
    let at = lr_core::Timestamp(1_700_000_010_000);
    assert_eq!(
        lr_exec::cancel_line("build", at, Some("bob at 12:00: stale\nx")),
        format!("=== step build cancelled at {at} | reason=bob at 12:00: stale x ===\n")
    );
    assert_eq!(
        lr_exec::cancel_line("build", at, None),
        format!("=== step build cancelled at {at} ===\n")
    );
}
