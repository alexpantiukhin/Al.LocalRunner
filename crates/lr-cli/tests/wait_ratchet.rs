use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    eventually: usize,
    sleep: usize,
    timeouts: usize,
}

const THRESHOLD: Counts = Counts {
    eventually: 191,
    sleep: 64,
    timeouts: 64,
};

const LONG_WAITS: [&str; 5] = ["30", "45", "60", "90", "120"];

impl Counts {
    fn plus(self, o: Counts) -> Counts {
        Counts {
            eventually: self.eventually + o.eventually,
            sleep: self.sleep + o.sleep,
            timeouts: self.timeouts + o.timeouts,
        }
    }

    fn above(&self, limit: &Counts) -> bool {
        self.eventually > limit.eventually
            || self.sleep > limit.sleep
            || self.timeouts > limit.timeouts
    }

    fn is_zero(&self) -> bool {
        *self == Counts::default()
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "eventually: {}, sleep: {}, timeouts: {}",
            self.eventually, self.sleep, self.timeouts
        )
    }
}

fn ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn ident_before(b: &[u8], i: usize) -> bool {
    i > 0 && ident_byte(b[i - 1])
}

fn clear(out: &mut [u8], from: usize, to: usize) {
    let end = to.min(out.len());
    for c in &mut out[from..end] {
        if *c != b'\n' {
            *c = b' ';
        }
    }
}

fn raw_string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    if b.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j - i - 1;
    let mut k = j + 1;
    while k < b.len() {
        if b[k] == b'"' && b[k + 1..].starts_with(&vec![b'#'; hashes]) {
            return Some(k + 1 + hashes);
        }
        k += 1;
    }
    Some(b.len())
}

fn blank(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == b'/' && next == Some(b'/') {
            let end = b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |p| i + p);
            clear(&mut out, i, end);
            i = end;
        } else if c == b'/' && next == Some(b'*') {
            let mut depth = 0;
            let mut j = i;
            while j < b.len() {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            clear(&mut out, i, j);
            i = j;
        } else if c == b'r'
            && (!ident_before(b, i) || (b[i - 1] == b'b' && !ident_before(b, i - 1)))
            && raw_string_end(b, i).is_some()
        {
            let end = raw_string_end(b, i).unwrap_or(b.len());
            clear(&mut out, i, end);
            i = end;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            clear(&mut out, i, j + 1);
            i = j + 1;
        } else if c == b'\'' {
            if next == Some(b'\\') {
                let mut j = i + 3;
                while j < b.len() && b[j] != b'\'' {
                    j += 1;
                }
                clear(&mut out, i, j + 1);
                i = j + 1;
            } else {
                let width = src[i + 1..].chars().next().map_or(1, char::len_utf8);
                if b.get(i + 1 + width) == Some(&b'\'') {
                    clear(&mut out, i, i + 2 + width);
                    i += 2 + width;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

fn words(code: &[u8], word: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + word.len() <= code.len() {
        if &code[i..i + word.len()] == word
            && !ident_before(code, i)
            && !code.get(i + word.len()).is_some_and(|&c| ident_byte(c))
        {
            found.push(i);
            i += word.len();
        } else {
            i += 1;
        }
    }
    found
}

fn skip_space(code: &[u8], mut i: usize) -> usize {
    while code.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
        i += 1;
    }
    i
}

fn space_back(code: &[u8], mut i: usize) -> usize {
    while i > 0 && code[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

fn ident_start(code: &[u8], mut i: usize) -> usize {
    while ident_before(code, i) {
        i -= 1;
    }
    i
}

fn closing(code: &[u8], open: usize) -> usize {
    let (up, down) = match code[open] {
        b'{' => (b'{', b'}'),
        b'[' => (b'[', b']'),
        _ => (b'(', b')'),
    };
    let mut depth = 0usize;
    for (k, &c) in code.iter().enumerate().skip(open) {
        if c == up {
            depth += 1;
        } else if c == down {
            depth -= 1;
            if depth == 0 {
                return k;
            }
        }
    }
    code.len()
}

fn after_fn(code: &[u8], at: usize) -> bool {
    let end = space_back(code, at);
    end >= 2 && &code[end - 2..end] == b"fn" && !ident_before(code, end - 2)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn paused_ranges(code: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for at in words(code, b"fn") {
        let name = skip_space(code, at + 2);
        if !code.get(name).is_some_and(|&c| ident_byte(c)) {
            continue;
        }
        let Some(open) = code[name..]
            .iter()
            .position(|&c| c == b'{' || c == b';')
            .map(|p| name + p)
            .filter(|&p| code[p] == b'{')
        else {
            continue;
        };
        let close = closing(code, open);
        let head_start = code[..at]
            .iter()
            .rposition(|&c| c == b'}' || c == b';' || c == b'{')
            .map_or(0, |p| p + 1);
        if contains(&code[head_start..at], b"start_paused")
            || contains(&code[open..close], b"time::pause(")
        {
            ranges.push((open, close));
        }
    }
    ranges
}

fn assert_ranges(code: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < code.len() {
        if ident_byte(code[i]) && !ident_before(code, i) {
            let mut end = i;
            while code.get(end).is_some_and(|&c| ident_byte(c)) {
                end += 1;
            }
            let name = &code[i..end];
            let bang = skip_space(code, end);
            if (name.starts_with(b"assert") || name.starts_with(b"debug_assert"))
                && code.get(bang) == Some(&b'!')
            {
                let open = skip_space(code, bang + 1);
                if matches!(code.get(open), Some(b'(' | b'[' | b'{')) {
                    ranges.push((open, closing(code, open)));
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    ranges
}

fn inside(ranges: &[(usize, usize)], at: usize) -> bool {
    ranges.iter().any(|&(a, b)| a <= at && at <= b)
}

fn called(code: &[u8], end: usize) -> bool {
    code.get(skip_space(code, end)) == Some(&b'(')
}

fn clock_argument(code: &[u8], at: usize) -> bool {
    let mut i = space_back(code, at);
    while i >= 2 && &code[i - 2..i] == b"::" {
        i = space_back(code, ident_start(code, i - 2));
    }
    if i == 0 || code[i - 1] != b'(' {
        return false;
    }
    let end = space_back(code, i - 1);
    let start = ident_start(code, end);
    let name = &code[start..end];
    let method = space_back(code, start) > 0 && code[space_back(code, start) - 1] == b'.';
    name == b"advance" || (name == b"sleep" && method)
}

fn count(src: &str) -> Counts {
    let code = blank(src);
    let paused = paused_ranges(&code);
    let asserts = assert_ranges(&code);
    let mut counts = Counts::default();
    for at in words(&code, b"eventually") {
        if called(&code, at + 10) && !after_fn(&code, at) && !inside(&paused, at) {
            counts.eventually += 1;
        }
    }
    let mut i = 0;
    while let Some(p) = code[i..].windows(5).position(|w| w == b"sleep") {
        let at = i + p;
        i = at + 5;
        if code.get(at + 5).is_some_and(|&c| ident_byte(c)) || !called(&code, at + 5) {
            continue;
        }
        let start = ident_start(&code, at);
        let before = space_back(&code, start);
        let method = before > 0 && code[before - 1] == b'.';
        if !method && !after_fn(&code, start) && !inside(&paused, at) {
            counts.sleep += 1;
        }
    }
    for at in words(&code, b"from_secs") {
        let open = skip_space(&code, at + 9);
        if code.get(open) != Some(&b'(') {
            continue;
        }
        let close = closing(&code, open);
        let literal = String::from_utf8_lossy(&code[open + 1..close]);
        if LONG_WAITS.contains(&literal.trim())
            && !inside(&paused, at)
            && !inside(&asserts, at)
            && !clock_argument(&code, at)
        {
            counts.timeouts += 1;
        }
    }
    counts
}

fn test_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            test_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn workspace_counts() -> Vec<(PathBuf, Counts)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory")
        .to_path_buf();
    let mut members: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("the crates directory is readable")
        .map(|e| e.expect("directory entry").path().join("tests"))
        .filter(|p| p.is_dir())
        .collect();
    members.sort();
    let mut files = Vec::new();
    for tests in members {
        test_sources(&tests, &mut files);
    }
    files
        .into_iter()
        .map(|path| {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let rel = path.strip_prefix(&crates).unwrap_or(&path).to_path_buf();
            (rel, count(&src))
        })
        .collect()
}

#[test]
fn wait_ratchet_counts_call_sites_not_definitions() {
    let fixture = r##"
pub async fn eventually<T, F>(what: &str, probe: F) -> T { todo!() }
async fn real_sleep(ms: u64) { tokio::time::sleep(Duration::from_millis(ms)).await }

#[tokio::test]
async fn waits() {
    eventually("ready", Duration::from_secs(30), || async { Some(()) }).await;
    support::eventually ("named", Duration::from_secs(120), probe).await;
    std::thread::sleep(Duration::from_millis(50));
    real_sleep(400).await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(45), work()).await;
    let _ = Duration::from_secs(31);
    // eventually("commented", Duration::from_secs(30), probe).await; sleep(x);
    let text = "eventually( sleep( from_secs(30)";
    let raw = r#"thread::sleep(Duration::from_secs(90))"#;
    let c = '"';
    let lifetime: &'static str = "x";
    clock.sleep(Duration::from_secs(60)).await;
    h.clock.advance(Duration::from_secs(90));
    assert!(took < Duration::from_secs(30), "{c}{lifetime}");
    use support::eventually;
}

#[tokio::test(start_paused = true)]
async fn paused_by_the_runtime() {
    tokio::time::sleep(Duration::from_secs(60)).await;
    eventually("paused", Duration::from_secs(30), probe).await;
}

#[tokio::test]
async fn paused_by_hand() {
    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(90)).await;
}
"##;
    assert_eq!(
        count(fixture),
        Counts {
            eventually: 2,
            sleep: 3,
            timeouts: 4,
        }
    );
    assert!(count("fn sleep(d: Duration) {}\nfn eventually() {}\n").is_zero());
}

#[test]
fn source_does_not_wait_on_real_time_more_than_the_threshold() {
    let files = workspace_counts();
    assert!(
        files.len() > 100,
        "the walk found {} test sources",
        files.len()
    );
    let total = files
        .iter()
        .fold(Counts::default(), |sum, (_, c)| sum.plus(*c));
    let listing = || {
        files
            .iter()
            .filter(|(_, c)| !c.is_zero())
            .map(|(p, c)| format!("  {}: {c}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !total.above(&THRESHOLD),
        "tests wait on real time more than allowed ({total} > {THRESHOLD}); wait on an event the \
         code produces or a manual clock instead (AGENTS.md), and lower the threshold in the same \
         commit as you remove waits. Files:\n{}",
        listing()
    );
    assert_eq!(
        total, THRESHOLD,
        "fewer waits on real time than the threshold: lower the threshold in the same commit as \
         you remove waits, to {total}"
    );
}
