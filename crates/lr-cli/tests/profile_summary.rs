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
    assert_eq!(p.platform_mode, "no platforms");
    let l = lines(text);
    assert!(
        l.contains(&"profile unit: no platforms, timeout 900 s".to_string()),
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
fn platforms_and_platform_tables_mean_with_platforms() {
    let texts = [
        "[profile.p]\ncommand = \"true\"\ntimeout_s = 5\nplatforms = [\"linux\"]\n",
        "[profile.p]\ncommand = \"true\"\ntimeout_s = 5\n[profile.p.platform.ios]\ncommand = \"true\"\n",
        "[needs.n]\nplatforms = [\"linux\"]\n[profile.p]\ncommand = \"true\"\ntimeout_s = 5\nneeds = \"n\"\n",
    ];
    for text in texts {
        let p = summarize(&parse_profiles(text).unwrap()[0]);
        assert_eq!(p.platform_mode, "with platforms", "{text}");
    }
    assert!(lines(texts[0])[0].contains("with platforms (linux)"));
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

const GLUED: &str = "[profile.t]
command = \"cargo test --no-fail-fast{? -p {pkg}}\"
timeout_s = 60
[profile.t.args.pkg]
one_of = [\"core\", \"cli\"]
";

#[test]
fn an_optional_segment_is_shown_expanded_with_every_argument_and_with_none() {
    let l = lines(GLUED);
    assert!(
        l.contains(
            &"  expanded, all arguments given: cargo test --no-fail-fast-p core".to_string()
        ),
        "{l:?}"
    );
    assert!(
        l.contains(&"  expanded, no argument given: cargo test --no-fail-fast".to_string()),
        "{l:?}"
    );
}

#[test]
fn a_required_argument_that_is_missing_is_reported_in_the_no_argument_case() {
    let text = "[profile.t]
command = \"cargo test {pkg}{? -- {filter}}\"
timeout_s = 60
[profile.t.args.pkg]
pattern = \"[a-z]+\"
[profile.t.args.filter]
pattern = \"[a-z]+\"
";
    let l = lines(text);
    assert!(
        l.contains(&"  expanded, all arguments given: cargo test <pkg>-- <filter>".to_string()),
        "{l:?}"
    );
    assert!(
        l.iter()
            .any(|x| x.starts_with("  expanded, no argument given: ")),
        "{l:?}"
    );
}

#[test]
fn a_profile_without_an_optional_segment_has_no_expansions() {
    let text = "[profile.t]
command = \"cargo test {pkg}\"
timeout_s = 60
[profile.t.args.pkg]
default = \"core\"
";
    assert!(summarize(&parse_profiles(text).unwrap()[0])
        .expansions
        .is_empty());
    assert!(!lines(text).iter().any(|x| x.contains("expanded")));
}

#[test]
fn zone_variants_and_declared_resources_are_shown() {
    let text = "[profile.hello]
command = \"echo hello local\"
timeout_s = 60
tags = [\"vps\"]
exclusive = [\"gpu\"]
programs = [\"git\"]
[profile.hello.zone.remote]
command = \"echo hello remote\"
";
    let l = lines(text);
    assert!(
        l.contains(&"  zone remote: echo hello remote".to_string()),
        "{l:?}"
    );
    assert!(
        l.contains(&"  resources: tags vps, exclusive gpu, programs git".to_string()),
        "{l:?}"
    );
    assert!(!l.iter().any(|x| x.contains("none declared")), "{l:?}");
    let v = serde_json::to_value(summarize(&parse_profiles(text).unwrap()[0])).unwrap();
    assert_eq!(v["tags"], serde_json::json!(["vps"]));
    assert_eq!(v["zones"][0]["zone"], "remote");
}

#[test]
fn zone_variant_with_steps_lists_each_step() {
    let text = "[profile.g]
timeout_s = 60
steps = [{ name = \"a\", command = \"x\", timeout_s = 5 }]
[profile.g.zone.lan]
steps = [{ name = \"a\", command = \"y\", timeout_s = 5 }]
";
    let l = lines(text);
    assert!(
        l.contains(&"  zone lan, step a: y (timeout 5 s)".to_string()),
        "{l:?}"
    );
}
