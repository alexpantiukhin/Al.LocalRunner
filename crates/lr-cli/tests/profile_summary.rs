use lr_cli::commands::profile_summary::{summarize, summary_lines, verdict};
use lr_core::parse_profiles;

fn lines(text: &str) -> Vec<String> {
    let list: Vec<_> = parse_profiles(text)
        .unwrap()
        .iter()
        .map(summarize)
        .collect();
    summary_lines(&list)
}

#[test]
fn a_command_profile_is_summarized() {
    let text =
        "[profile.unit]\ncommand = \"cargo test\"\ntimeout_s = 900\nmem_mb = 4096\ncpu = 2\n";
    let p = summarize(&parse_profiles(text).unwrap()[0]);
    assert_eq!(p.command.as_deref(), Some("cargo test"));
    assert!(p.steps.is_empty());
    assert_eq!(p.platform_mode, "by work tree");
    let l = lines(text);
    assert!(
        l.contains(&"profile unit: by work tree, timeout 900 s".to_string()),
        "{l:?}"
    );
    assert!(l.contains(&"  command: cargo test".to_string()));
    assert!(l.contains(&"  resources: mem 4096 MB, cpu 2".to_string()));
}

#[test]
fn a_steps_profile_lists_every_step() {
    let text = "[profile.gate]\ntimeout_s = 600\nsteps = [\n  { name = \"lint\", command = \"cargo clippy\", timeout_s = 60 },\n  { name = \"test\", command = \"cargo test\", timeout_s = 300 },\n]\n";
    let l = lines(text);
    assert!(
        l.contains(&"  step lint: cargo clippy (timeout 60 s)".to_string()),
        "{l:?}"
    );
    assert!(l.contains(&"  step test: cargo test (timeout 300 s)".to_string()));
}

#[test]
fn platforms_and_platform_tables_mean_by_commit() {
    let texts = [
        "[profile.p]\ncommand = \"true\"\ntimeout_s = 5\nplatforms = [\"linux\"]\n",
        "[profile.p]\ncommand = \"true\"\ntimeout_s = 5\n[profile.p.platform.ios]\ncommand = \"true\"\n",
        "[needs.n]\nplatforms = [\"linux\"]\n[profile.p]\ncommand = \"true\"\ntimeout_s = 5\nneeds = \"n\"\n",
    ];
    for text in texts {
        let p = summarize(&parse_profiles(text).unwrap()[0]);
        assert_eq!(p.platform_mode, "by commit", "{text}");
    }
    assert!(lines(texts[0])[0].contains("by commit (linux)"));
}

#[test]
fn arguments_and_missing_resources() {
    let text =
        "[profile.t]\ncommand = \"cargo test {target}\"\ntimeout_s = 60\n[profile.t.args.target]\n";
    let l = lines(text);
    assert!(l.contains(&"  arguments: target".to_string()), "{l:?}");
    assert!(l.contains(&"  resources: none declared".to_string()));
}

#[test]
fn the_summary_serializes_with_snake_case_keys() {
    let text = "[profile.t]\ncommand = \"true\"\ntimeout_s = 5\n";
    let v = serde_json::to_value(summarize(&parse_profiles(text).unwrap()[0])).unwrap();
    assert!(v.get("platform_mode").is_some() && v.get("timeout_s").is_some());
}

#[test]
fn the_verdict_needs_a_profile_and_good_secrets() {
    let text = "[profile.t]\ncommand = \"true\"\ntimeout_s = 5\n";
    let profiles = parse_profiles(text).unwrap();
    assert!(!verdict(&[], true));
    assert!(!verdict(&profiles, false));
    assert!(verdict(&profiles, true));
}

#[test]
fn labels_are_listed_and_serialized() {
    let text = "[profile.t]\ncommand = \"true\"\ntimeout_s = 5\nlabels = [\"work\", \"release\"]\n";
    let l = lines(text);
    assert!(l.contains(&"  labels: release, work".to_string()), "{l:?}");
    let v = serde_json::to_value(summarize(&parse_profiles(text).unwrap()[0])).unwrap();
    assert_eq!(v["labels"], serde_json::json!(["release", "work"]));
}
