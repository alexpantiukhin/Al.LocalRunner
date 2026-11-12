use lr_core::{parse_profiles, resolve_for_platform, Profile, ProfileError, StepState, Zone};

const SPEC_EXAMPLES: &str = r#"
[profile.rust-unit]
command = "cargo test --workspace"
timeout_s = 900
mem_mb = 4096
cpu = 4
tags = ["cargo"]

[profile.android-tier]
command = "pwsh scripts/android-tier.ps1"
timeout_s = 3600
mem_mb = 12288
cpu = 6
tags = ["gradle", "emulator"]
exclusive = ["gradle", "emulator"]
"#;

const GATE: &str = r#"
[profile.rust-gate]
timeout_s = 1800
tags = ["cargo"]
steps = [
  { name = "fmt",    command = "cargo fmt --all --check", timeout_s = 120 },
  { name = "clippy", command = "cargo clippy --workspace --all-targets -- -D warnings", timeout_s = 600 },
  { name = "test",   command = "cargo test --workspace", timeout_s = 900, mem_mb = 8192 },
]
"#;

fn one(text: &str) -> Profile {
    parse_profiles(text).unwrap().remove(0)
}

fn invalid(text: &str) -> (String, String) {
    match parse_profiles(text).unwrap_err() {
        ProfileError::Invalid { profile, field, .. } => (profile, field),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn spec_example_profiles_parse() {
    let profiles = parse_profiles(SPEC_EXAMPLES).unwrap();
    assert_eq!(profiles.len(), 2);
    let android = profiles.iter().find(|p| p.name == "android-tier").unwrap();
    assert_eq!(android.mem_mb, Some(12288));
    assert_eq!(android.cpu, Some(6));
    assert_eq!(android.timeout_s, 3600);
    assert_eq!(android.tags, ["gradle", "emulator"]);
    assert_eq!(android.exclusive, ["gradle", "emulator"]);
    let unit = profiles.iter().find(|p| p.name == "rust-unit").unwrap();
    assert_eq!(unit.mem_mb, Some(4096));
}

#[test]
fn step_example_profile_parses() {
    let p = one(GATE);
    assert_eq!(p.steps.len(), 3);
    assert_eq!(p.steps[2].mem_mb, Some(8192));
    assert_eq!(p.steps[1].name, "clippy");
}

#[test]
fn command_and_steps_together_rejected() {
    let text = r#"
[profile.both]
command = "a"
timeout_s = 10
steps = [{ name = "s", command = "b", timeout_s = 5 }]
"#;
    assert_eq!(invalid(text), ("both".into(), "command".into()));
}

#[test]
fn neither_command_nor_steps_rejected() {
    let text = "[profile.none]
timeout_s = 10
";
    assert_eq!(invalid(text), ("none".into(), "command".into()));
}

#[test]
fn missing_timeout_rejected() {
    let text = "[profile.p]
command = \"a\"
";
    assert_eq!(invalid(text), ("p".into(), "timeout_s".into()));
}

#[test]
fn missing_step_timeout_rejected() {
    let text = r#"
[profile.p]
timeout_s = 100
steps = [{ name = "a", command = "x", timeout_s = 5 }, { name = "b", command = "y" }]
"#;
    assert_eq!(invalid(text), ("p".into(), "steps[1].timeout_s".into()));
}

#[test]
fn zero_timeout_rejected_as_no_unlimited_run() {
    let text = "[profile.p]
command = \"a\"
timeout_s = 0
";
    assert_eq!(invalid(text), ("p".into(), "timeout_s".into()));
}

#[test]
fn zero_step_timeout_rejected() {
    let text = "[profile.p]
timeout_s = 9
steps = [{ name = \"a\", command = \"x\", timeout_s = 0 }]
";
    assert_eq!(invalid(text), ("p".into(), "steps[0].timeout_s".into()));
}

#[test]
fn empty_steps_rejected() {
    let text = "[profile.p]
timeout_s = 9
steps = []
";
    assert_eq!(invalid(text), ("p".into(), "steps".into()));
}

#[test]
fn empty_command_rejected() {
    let text = "[profile.p]
timeout_s = 9
command = \"  \"
";
    assert_eq!(invalid(text), ("p".into(), "command".into()));
}

#[test]
fn unknown_zone_rejected() {
    let text = "[profile.p]
timeout_s = 9
command = \"a\"
zone = \"moon\"
";
    assert_eq!(invalid(text), ("p".into(), "zone".into()));
}

#[test]
fn unknown_key_rejected_with_profile_name() {
    let text = "[profile.p]
timeout_s = 9
command = \"a\"
level = \"heavy\"
";
    assert_eq!(invalid(text).0, "p");
}

#[test]
fn broken_toml_is_a_syntax_error() {
    assert!(matches!(
        parse_profiles(
            "[profile.p
"
        ),
        Err(ProfileError::Syntax(_))
    ));
}

#[test]
fn single_command_becomes_one_step() {
    let p = one("[profile.p]
command = \"cargo test\"
timeout_s = 900
");
    assert_eq!(p.steps.len(), 1);
    assert_eq!(p.steps[0].command, "cargo test");
    assert_eq!(p.steps[0].timeout_s, 900);
    assert!(p.from_command);
}

#[test]
fn profile_without_platforms_resolves_for_any_platform() {
    let p = one("[profile.p]
command = \"a\"
timeout_s = 9
");
    assert!(resolve_for_platform(&p, "anything").is_ok());
}

const MULTI: &str = r#"
[profile.app]
command = "build-all"
timeout_s = 600
mem_mb = 2048

[profile.app.platform.ios]
command = "xcodebuild"
timeout_s = 1200
mem_mb = 8192

[profile.app.platform.android]
steps = [{ name = "g", command = "gradle assemble", timeout_s = 300 }]
"#;

#[test]
fn platform_override_replaces_command() {
    let p = one(MULTI);
    let ios = resolve_for_platform(&p, "ios").unwrap();
    assert_eq!(ios.steps.len(), 1);
    assert_eq!(ios.steps[0].command, "xcodebuild");
    assert_eq!(ios.steps[0].timeout_s, 1200);
    assert_eq!(ios.timeout_s, 1200);
    assert_eq!(ios.mem_mb, Some(8192));
    assert_eq!(ios.platforms, ["ios"]);
    assert!(ios.platform_overrides.is_empty());
}

#[test]
fn platform_override_replaces_steps() {
    let p = one(MULTI);
    let android = resolve_for_platform(&p, "android").unwrap();
    assert_eq!(android.steps[0].name, "g");
    assert!(!android.from_command);
    assert_eq!(android.timeout_s, 600);
}

#[test]
fn platform_override_with_command_and_steps_rejected() {
    let text = r#"
[profile.p]
command = "a"
timeout_s = 9
[profile.p.platform.ios]
command = "b"
steps = [{ name = "s", command = "c", timeout_s = 1 }]
"#;
    assert_eq!(invalid(text), ("p".into(), "platform.ios.command".into()));
}

#[test]
fn platform_without_override_keeps_profile_command() {
    let p = one("[profile.p]
command = \"a\"
timeout_s = 9
platforms = [\"linux\", \"macos\"]
");
    let r = resolve_for_platform(&p, "macos").unwrap();
    assert_eq!(r.steps[0].command, "a");
}

#[test]
fn unsupported_platform_rejected() {
    let p = one(MULTI);
    assert_eq!(
        resolve_for_platform(&p, "windows").unwrap_err(),
        ProfileError::UnsupportedPlatform {
            profile: "app".into(),
            platform: "windows".into()
        }
    );
}

#[test]
fn listed_platforms_reject_unlisted_platform() {
    let p = one("[profile.p]
command = \"a\"
timeout_s = 9
platforms = [\"linux\"]
");
    assert!(resolve_for_platform(&p, "macos").is_err());
}

#[test]
fn step_timeout_inside_job_timeout() {
    let p = one(GATE);
    let test_step = &p.steps[2];
    assert_eq!(test_step.effective_timeout_s(1800), 900);
    assert_eq!(test_step.effective_timeout_s(300), 300);
}

#[test]
fn step_override_of_tags_and_cpu_parses() {
    let text = r#"
[profile.p]
timeout_s = 100
tags = ["a"]
steps = [{ name = "s", command = "x", timeout_s = 5, cpu = 2, tags = ["b"], secrets = ["k"] }]
"#;
    let s = &one(text).steps[0];
    assert_eq!(s.cpu, Some(2));
    assert_eq!(s.tags.as_deref(), Some(&["b".to_string()][..]));
    assert_eq!(s.secrets, ["k"]);
}

const FAILING: &str = r#"
[profile.p]
timeout_s = 100
steps = [
  { name = "a", command = "x", timeout_s = 5 },
  { name = "b", command = "y", timeout_s = 5 },
  { name = "c", command = "z", timeout_s = 5 },
]
"#;

#[test]
fn failed_step_skips_the_rest() {
    let p = one(FAILING);
    assert_eq!(p.skipped_after_failure(0), [1, 2]);
    assert_eq!(
        p.states_after_failure(1),
        [StepState::Passed, StepState::Failed, StepState::Skipped]
    );
}

#[test]
fn continue_on_failure_keeps_the_rest_queued() {
    let text = r#"
[profile.p]
timeout_s = 100
steps = [
  { name = "a", command = "x", timeout_s = 5, continue_on_failure = true },
  { name = "b", command = "y", timeout_s = 5 },
]
"#;
    let p = one(text);
    assert!(p.skipped_after_failure(0).is_empty());
    assert_eq!(
        p.states_after_failure(0),
        [StepState::Failed, StepState::Queued]
    );
}

#[test]
fn needs_set_supplies_fields_and_profile_overrides_them() {
    let text = r#"
[needs.heavy]
mem_mb = 8192
cpu = 4
tags = ["gradle"]

[profile.a]
needs = "heavy"
command = "x"
timeout_s = 10

[profile.b]
needs = "heavy"
command = "x"
timeout_s = 10
mem_mb = 16384
"#;
    let ps = parse_profiles(text).unwrap();
    assert_eq!((ps[0].mem_mb, ps[0].cpu), (Some(8192), Some(4)));
    assert_eq!(ps[0].tags, ["gradle"]);
    assert_eq!((ps[1].mem_mb, ps[1].cpu), (Some(16384), Some(4)));
}

#[test]
fn needs_set_can_supply_timeout() {
    let text = "[needs.n]
timeout_s = 77
[profile.a]
needs = \"n\"
command = \"x\"
";
    assert_eq!(one(text).timeout_s, 77);
}

#[test]
fn unknown_needs_set_rejected() {
    let text = "[profile.a]
needs = \"ghost\"
command = \"x\"
timeout_s = 1
";
    assert_eq!(invalid(text), ("a".into(), "needs".into()));
}

#[test]
fn programs_accept_names_and_checked_entries() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 1
programs = ["git", "adb", { name = "gradle", check = "gradle --version" }]
"#;
    let p = one(text);
    assert_eq!(p.programs.len(), 3);
    assert_eq!(p.programs[0].check, None);
    assert_eq!(p.programs[2].name, "gradle");
    assert_eq!(p.programs[2].check.as_deref(), Some("gradle --version"));
}

#[test]
fn project_fields_cache_services_secrets_zone_parse() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 1
cache = ["gradle", "cargo"]
services = ["build-daemon"]
secrets = ["registry_auth"]
zone = "lan"
disk_mb = 51200
"#;
    let p = one(text);
    assert_eq!(p.cache, ["gradle", "cargo"]);
    assert_eq!(p.services, ["build-daemon"]);
    assert_eq!(p.secrets, ["registry_auth"]);
    assert_eq!(p.zone, Some(Zone::Lan));
    assert_eq!(p.disk_mb, Some(51200));
}

#[test]
fn override_with_only_timeout_moves_single_step_timeout() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 100
[profile.p.platform.ios]
timeout_s = 500
"#;
    let r = resolve_for_platform(&one(text), "ios").unwrap();
    assert_eq!(r.timeout_s, 500);
    assert_eq!(r.steps[0].timeout_s, 500);
}

#[test]
fn override_replaces_cpu_and_disk() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 100
cpu = 1
disk_mb = 10
[profile.p.platform.ios]
cpu = 8
disk_mb = 9000
"#;
    let r = resolve_for_platform(&one(text), "ios").unwrap();
    assert_eq!((r.cpu, r.disk_mb), (Some(8), Some(9000)));
}

#[test]
fn zero_timeout_in_platform_override_rejected() {
    let text = "[profile.p]
command = \"x\"
timeout_s = 9
[profile.p.platform.ios]
timeout_s = 0
";
    assert_eq!(invalid(text), ("p".into(), "platform.ios.timeout_s".into()));
}

#[test]
fn unknown_key_in_needs_set_rejected() {
    let text = "[needs.n]
level = \"x\"
[profile.p]
command = \"x\"
timeout_s = 1
";
    assert_eq!(invalid(text), ("needs.n".into(), "level".into()));
}

#[test]
fn needs_set_supplies_platforms() {
    let text = "[needs.n]
platforms = [\"linux\"]
[profile.p]
needs = \"n\"
command = \"x\"
timeout_s = 1
";
    let p = one(text);
    assert_eq!(p.platforms, ["linux"]);
    assert!(resolve_for_platform(&p, "macos").is_err());
}

#[test]
fn duplicate_step_names_rejected() {
    let text = r#"
[profile.p]
timeout_s = 100
steps = [
  { name = "a", command = "x", timeout_s = 5 },
  { name = "a", command = "y", timeout_s = 5 },
]
"#;
    assert_eq!(invalid(text), ("p".into(), "steps[1].name".into()));
}
