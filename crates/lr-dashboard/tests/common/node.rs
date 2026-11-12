use serde_json::Value;
use std::io::Write;

/// Runs `program` in node (fed on stdin, so its size is not bound by the command line) and
/// parses what it prints as JSON. Without node the test is skipped, unless
/// `LOCALRUNNER_REQUIRE_TEST_TOOLS` names `node` or `all`: then it fails.
pub fn run(program: &str, test: &str) -> Option<Value> {
    let child = std::process::Command::new("node")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let out = match child {
        Ok(mut child) => {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(program.as_bytes()).unwrap();
            drop(stdin);
            child.wait_with_output().unwrap()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let why = format!("node is not available ({e}), page code not executed");
            let required = std::env::var("LOCALRUNNER_REQUIRE_TEST_TOOLS").unwrap_or_default();
            assert!(
                !required
                    .split(',')
                    .any(|t| t.trim() == "node" || t.trim() == "all"),
                "LOCALRUNNER_REQUIRE_TEST_TOOLS={required} requires node, but {why}"
            );
            let _ = writeln!(std::io::stderr(), "SKIPPED {test}: {why}");
            return None;
        }
        Err(e) => panic!("node did not start: {e}"),
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

/// The source of `function <name>(` in `script`, up to its balanced closing brace, with line
/// ends as LF whatever the checkout has.
pub fn function(script: &str, name: &str) -> String {
    let text = script.replace("\r\n", "\n");
    let start = text
        .find(&format!("function {name}("))
        .unwrap_or_else(|| panic!("function {name} in the page code"));
    let open = start + text[start..].find('{').unwrap();
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return text[start..open + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("function {name} has no closing brace")
}
