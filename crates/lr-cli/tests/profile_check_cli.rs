mod common;

use std::path::Path;
use std::process::{Command, Output};

const GOOD: &str = "[profile.unit]\ncommand = \"cargo test\"\ntimeout_s = 900\n";

fn check(dir: &Path, text: &str, extra: &[&str]) -> Output {
    let file = dir.join("profiles.toml");
    std::fs::write(&file, text).unwrap();
    Command::new(env!("CARGO_BIN_EXE_localrunner"))
        .args(["profile", "check", "--file"])
        .arg(&file)
        .args(extra)
        .env("LOCALRUNNER_STATE_DIR", dir.join("state"))
        .output()
        .expect("the binary runs")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_good_file_prints_the_summary_then_the_secrets_report_and_exits_0() {
    let dir = common::TempDir::new("good");
    let o = check(dir.path(), GOOD, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    let summary = text
        .find("profile unit: by work tree, timeout 900 s")
        .expect(&text);
    let secrets = text.find("no profile asks for a secret").expect(&text);
    assert!(summary < secrets);
}

#[test]
fn zero_profiles_fails() {
    let dir = common::TempDir::new("zero");
    let o = check(dir.path(), "# nothing\n", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("no profiles in"), "{}", out(&o));
}

#[test]
fn a_typo_section_warns_and_fails_on_zero_profiles() {
    let dir = common::TempDir::new("typo");
    let o = check(
        dir.path(),
        "[profiles.x]\ncommand = \"true\"\ntimeout_s = 5\n",
        &[],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).contains("ignored top-level section [profiles]"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("[profile.<name>]"));
}

#[test]
fn a_typo_section_next_to_a_good_profile_warns_and_exits_0() {
    let dir = common::TempDir::new("typo-good");
    let text = format!("{GOOD}[profiles.x]\ncommand = \"true\"\ntimeout_s = 5\n");
    let o = check(dir.path(), &text, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ignored top-level section [profiles]"));
}

#[test]
fn a_syntax_error_fails_with_the_parser_message_on_stderr() {
    let dir = common::TempDir::new("syntax");
    let o = check(dir.path(), "[profile", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("profiles.toml:"), "{}", err(&o));
}

#[test]
fn json_is_one_document() {
    let dir = common::TempDir::new("json");
    let text =
        "[profile.unit]\ncommand = \"cargo test\"\ntimeout_s = 900\nplatforms = [\"linux\"]\n";
    let o = check(dir.path(), text, &["--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect(&out(&o));
    assert_eq!(v["profiles"][0]["platform_mode"], "by commit");
    assert_eq!(v["ok"], true);
    assert_eq!(v["ignored_sections"], serde_json::json!([]));
    assert_eq!(v["secrets"]["ok"], true);
}

#[test]
fn json_for_zero_profiles_has_ok_false_and_exit_1() {
    let dir = common::TempDir::new("json-zero");
    let o = check(dir.path(), "", &["--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect(&out(&o));
    assert_eq!(v["ok"], false);
}

#[test]
fn json_errors_go_to_stderr_and_stdout_stays_empty() {
    let dir = common::TempDir::new("json-syntax");
    let o = check(dir.path(), "[profile", &["--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert!(!err(&o).is_empty());
}

#[test]
fn an_unknown_profile_name_keeps_the_old_error() {
    let dir = common::TempDir::new("unknown");
    let o = check(dir.path(), GOOD, &["unit2"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no profile unit2 in"), "{}", err(&o));
}
