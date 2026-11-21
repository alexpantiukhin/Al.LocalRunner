use lr_core::{
    cache_env_var, parse_profiles, satisfies, Accept, Capabilities, MismatchReason, Profile,
    ProfileError, Requirements, ServiceStatus, Zone,
};
use std::collections::{BTreeMap, BTreeSet};

fn one(text: &str) -> Profile {
    parse_profiles(text).unwrap().remove(0)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn runner() -> Capabilities {
    Capabilities {
        platforms: set(&["linux", "android"]),
        mem_mb: 16384,
        cpu: 8,
        disk_mb: 100_000,
        programs: set(&["git", "adb", "gradle"]),
        tags: set(&["gradle", "emulator", "cargo"]),
        max_job_timeout_s: Some(3600),
        services: BTreeMap::new(),
        zone: Zone::Local,
        machine: None,
        accept: Accept::default(),
        commands: BTreeSet::new(),
    }
}

fn req(text: &str, platform: Option<&str>) -> Requirements {
    Requirements::from_profile(&one(text), platform).unwrap()
}

const BASIC: &str = "[profile.p]\ncommand = \"x\"\ntimeout_s = 600\nmem_mb = 4096\ncpu = 2\n";

fn reasons(caps: &Capabilities, r: &Requirements) -> Vec<MismatchReason> {
    satisfies(caps, r).unwrap_err().reasons
}

#[test]
fn requirements_take_max_memory_and_union_tags() {
    let text = r#"
[profile.p]
timeout_s = 1800
tags = ["cargo"]
mem_mb = 1024
cpu = 1
exclusive = ["cargo"]
steps = [
  { name = "a", command = "x", timeout_s = 10, mem_mb = 8192, cpu = 2, tags = ["gradle"] },
  { name = "b", command = "y", timeout_s = 10, mem_mb = 2048, cpu = 6 },
  { name = "c", command = "z", timeout_s = 10 },
]
"#;
    let r = req(text, Some("linux"));
    assert_eq!(r.mem_mb, 8192);
    assert_eq!(r.cpu, 6);
    assert_eq!(r.tags, set(&["cargo", "gradle"]));
    assert_eq!(r.exclusive, set(&["cargo"]));
}

#[test]
fn step_without_override_inherits_profile_memory() {
    let text = r#"
[profile.p]
timeout_s = 100
mem_mb = 3000
steps = [{ name = "a", command = "x", timeout_s = 10 }]
"#;
    assert_eq!(req(text, Some("linux")).mem_mb, 3000);
}

#[test]
fn step_secrets_join_profile_secrets() {
    let text = r#"
[profile.p]
timeout_s = 100
secrets = ["reg"]
steps = [{ name = "a", command = "x", timeout_s = 10, secrets = ["sign"] }]
"#;
    assert_eq!(req(text, Some("linux")).secrets, set(&["reg", "sign"]));
}

#[test]
fn requirements_carry_platform_timeout_disk_cache_services_zone() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 777
disk_mb = 5000
cache = ["gradle"]
services = ["d"]
zone = "lan"
programs = ["git", "git", { name = "gradle", check = "gradle --version" }]
"#;
    let r = req(text, Some("linux"));
    assert_eq!(r.platform.as_deref(), Some("linux"));
    assert_eq!(r.timeout_s, 777);
    assert_eq!(r.disk_mb, 5000);
    assert_eq!(r.cache, ["gradle"]);
    assert_eq!(r.services, ["d"]);
    assert_eq!(
        r.zones,
        Some(BTreeSet::from([Zone::Local, Zone::Shared])),
        "the legacy zone is the limit; a profile without variants has steps for local and shared only"
    );
    assert_eq!(r.programs.len(), 2);
}

#[test]
fn platform_resolution_changes_requirements() {
    let text = r#"
[profile.p]
command = "x"
timeout_s = 100
mem_mb = 1000
[profile.p.platform.ios]
command = "y"
mem_mb = 9000
"#;
    assert_eq!(req(text, Some("ios")).mem_mb, 9000);
}

#[test]
fn unsupported_platform_fails_derivation() {
    let p = one("[profile.p]\ncommand = \"x\"\ntimeout_s = 1\nplatforms = [\"linux\"]\n");
    assert!(Requirements::from_profile(&p, Some("ios")).is_err());
}

#[test]
fn matching_runner_satisfies() {
    assert!(satisfies(&runner(), &req(BASIC, Some("linux"))).is_ok());
}

#[test]
fn platform_is_compared_for_equality_and_mismatch_is_reported() {
    let mut caps = runner();
    caps.platforms = set(&["macos"]);
    assert_eq!(
        reasons(&caps, &req(BASIC, Some("linux"))),
        [MismatchReason::Platform {
            needed: "linux".into()
        }]
    );
}

#[test]
fn runner_with_lower_timeout_ceiling_does_not_satisfy() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 7200\n",
        Some("linux"),
    );
    let err = satisfies(&runner(), &r).unwrap_err();
    assert_eq!(
        err.reasons,
        [MismatchReason::Timeout {
            needed: 7200,
            ceiling: 3600
        }]
    );
    assert_eq!(
        err.to_string(),
        "needs a timeout of 7200 s, the runner ceiling is 3600 s"
    );
}

#[test]
fn timeout_equal_to_ceiling_satisfies() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 3600\n",
        Some("linux"),
    );
    assert!(satisfies(&runner(), &r).is_ok());
}

#[test]
fn runner_without_ceiling_accepts_any_timeout() {
    let mut caps = runner();
    caps.max_job_timeout_s = None;
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 999999\n",
        Some("linux"),
    );
    assert!(satisfies(&caps, &r).is_ok());
}

#[test]
fn missing_program_reports_which() {
    let text =
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nprograms = [\"git\", \"adb\", \"xcrun\"]\n";
    let mut caps = runner();
    caps.programs = set(&["git"]);
    let err = satisfies(&caps, &req(text, Some("linux"))).unwrap_err();
    assert_eq!(
        err.reasons,
        [
            MismatchReason::Program { name: "adb".into() },
            MismatchReason::Program {
                name: "xcrun".into()
            }
        ]
    );
    assert!(err.to_string().contains("adb not found"));
}

#[test]
fn not_enough_memory_reports_numbers() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nmem_mb = 12288\n",
        Some("linux"),
    );
    let mut caps = runner();
    caps.mem_mb = 8192;
    let err = satisfies(&caps, &r).unwrap_err();
    assert_eq!(
        err.reasons,
        [MismatchReason::Memory {
            needed: 12288,
            available: 8192
        }]
    );
    assert!(err.to_string().contains("needs 12288 MB"));
}

#[test]
fn not_enough_cpu_and_disk_reported() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\ncpu = 16\ndisk_mb = 200000\n",
        Some("linux"),
    );
    assert_eq!(
        reasons(&runner(), &r),
        [
            MismatchReason::Cpu {
                needed: 16,
                available: 8
            },
            MismatchReason::Disk {
                needed: 200_000,
                available: 100_000
            }
        ]
    );
}

#[test]
fn missing_tag_reported_including_exclusive_ones() {
    let text = "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\ntags = [\"gradle\", \"ios-sim\"]\nexclusive = [\"gpu\"]\n";
    assert_eq!(
        reasons(&runner(), &req(text, Some("linux"))),
        [
            MismatchReason::Tag { name: "gpu".into() },
            MismatchReason::Tag {
                name: "ios-sim".into()
            }
        ]
    );
}

#[test]
fn all_mismatches_are_listed_together() {
    let text = "[profile.p]
command = \"x\"
timeout_s = 9999
mem_mb = 99999
programs = [\"nope\"]
";
    assert_eq!(
        reasons(&runner(), &req(text, Some("linux"))),
        [
            MismatchReason::Memory {
                needed: 99999,
                available: 16384
            },
            MismatchReason::Program {
                name: "nope".into()
            },
            MismatchReason::Timeout {
                needed: 9999,
                ceiling: 3600
            }
        ]
    );
}

#[test]
fn step_tags_add_to_profile_tags_when_every_step_sets_tags() {
    let text = r#"
[profile.p]
timeout_s = 100
tags = ["base"]
steps = [
  { name = "a", command = "x", timeout_s = 5, tags = ["t1"] },
  { name = "b", command = "y", timeout_s = 5, tags = ["t2"] },
]
"#;
    assert_eq!(req(text, Some("linux")).tags, set(&["base", "t1", "t2"]));
}

#[test]
fn missing_service_does_not_satisfy() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nservices = [\"build-daemon\"]\n",
        Some("linux"),
    );
    assert_eq!(
        reasons(&runner(), &r),
        [MismatchReason::ServiceMissing {
            name: "build-daemon".into()
        }]
    );
}

#[test]
fn unavailable_service_does_not_satisfy_but_available_does() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nservices = [\"build-daemon\"]\n",
        Some("linux"),
    );
    let mut caps = runner();
    caps.services
        .insert("build-daemon".into(), ServiceStatus::Unavailable);
    assert_eq!(
        reasons(&caps, &r),
        [MismatchReason::ServiceUnavailable {
            name: "build-daemon".into()
        }]
    );
    caps.services
        .insert("build-daemon".into(), ServiceStatus::Available);
    assert!(satisfies(&caps, &r).is_ok());
}

#[test]
fn runner_farther_than_profile_zone_does_not_satisfy() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nzone = \"local\"\n",
        Some("linux"),
    );
    let mut caps = runner();
    caps.zone = Zone::Lan;
    assert_eq!(
        reasons(&caps, &r),
        [MismatchReason::Zone {
            runner: Zone::Lan,
            allowed: Zone::up_to(Zone::Local)
        }]
    );
    caps.zone = Zone::Local;
    assert!(satisfies(&caps, &r).is_ok());
}

#[test]
fn a_zoned_profile_without_zone_variants_runs_in_local_and_shared_only() {
    let zoned = BASIC.replace(
        "cpu = 2",
        "cpu = 2
zones = [\"local\", \"shared\", \"lan\", \"remote\"]",
    );
    let r = req(&zoned, Some("linux"));
    assert_eq!(
        r.zones,
        Some([Zone::Local, Zone::Shared].into_iter().collect())
    );
    let mut caps = runner();
    for zone in [Zone::Local, Zone::Shared] {
        caps.zone = zone;
        assert!(satisfies(&caps, &r).is_ok());
    }
    for zone in [Zone::Lan, Zone::Remote] {
        caps.zone = zone;
        assert_eq!(
            reasons(&caps, &r),
            [MismatchReason::Zone {
                runner: zone,
                allowed: [Zone::Local, Zone::Shared].into_iter().collect()
            }]
        );
    }
}

const VARIANTS: &str = r#"
[profile.p]
command = "cargo test"
timeout_s = 600
mem_mb = 4096

[profile.p.zone.shared]
command = "docker exec b cargo test"

[profile.p.zone.lan]
steps = [{ name = "run", command = "git fetch && cargo test", timeout_s = 900 }]
"#;

#[test]
fn a_zone_variant_replaces_the_common_steps_in_its_zone_and_opens_it() {
    let p = one(VARIANTS);
    let steps = p.zone_steps_effective();
    assert_eq!(
        steps.keys().copied().collect::<Vec<_>>(),
        [Zone::Local, Zone::Shared, Zone::Lan]
    );
    assert_eq!(steps[&Zone::Local][0].command, "cargo test");
    assert_eq!(steps[&Zone::Shared][0].command, "docker exec b cargo test");
    assert_eq!(steps[&Zone::Lan][0].timeout_s, 900);
    let r = req(VARIANTS, Some("linux"));
    assert_eq!(
        r.zones,
        Some([Zone::Local, Zone::Shared, Zone::Lan].into_iter().collect())
    );
}

#[test]
fn a_profile_may_have_only_zone_variants() {
    let p = one("[profile.p]
timeout_s = 60
[profile.p.zone.lan]
command = \"x\"
");
    assert!(!p.common);
    assert_eq!(
        p.zone_steps_effective().keys().copied().collect::<Vec<_>>(),
        [Zone::Lan]
    );
}

#[test]
fn zone_variants_must_keep_the_step_names() {
    let text = "[profile.p]
command = \"x\"
timeout_s = 9
[profile.p.zone.lan]
steps = [{ name = \"other\", command = \"y\", timeout_s = 9 }]
";
    assert!(matches!(
        parse_profiles(text),
        Err(ProfileError::Invalid { .. })
    ));
}

#[test]
fn the_zones_key_narrows_the_zones_and_excludes_the_legacy_zone_key() {
    let narrowed = VARIANTS.replace(
        "mem_mb = 4096",
        "mem_mb = 4096
zones = [\"local\", \"lan\"]",
    );
    let r = req(&narrowed, Some("linux"));
    assert_eq!(
        r.zones,
        Some([Zone::Local, Zone::Lan].into_iter().collect())
    );
    let both = "[profile.p]
command = \"x\"
timeout_s = 9
zone = \"lan\"
zones = [\"local\"]
";
    assert!(matches!(
        parse_profiles(both),
        Err(ProfileError::Invalid { .. })
    ));
    let none = "[profile.p]
command = \"x\"
timeout_s = 9
zones = [\"lan\"]
";
    let p = one(none);
    assert!(Requirements::from_profile(&p, Some("linux")).is_err());
}

#[test]
fn light_order_does_not_take_reserved_heavy_runner() {
    let mut caps = runner();
    caps.accept.min_mem_mb = Some(8192);
    assert_eq!(
        reasons(&caps, &req(BASIC, Some("linux"))),
        [MismatchReason::BelowAccept {
            needed: 4096,
            min: 8192
        }]
    );
    let heavy = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nmem_mb = 8192\n",
        Some("linux"),
    );
    assert!(satisfies(&caps, &heavy).is_ok());
}

#[test]
fn secrets_and_cache_do_not_affect_placement() {
    let r = req(
        "[profile.p]\ncommand = \"x\"\ntimeout_s = 9\nsecrets = [\"s\"]\ncache = [\"c\"]\n",
        Some("linux"),
    );
    assert!(satisfies(&runner(), &r).is_ok());
}

#[test]
fn cache_variable_name_is_upper_cased() {
    assert_eq!(cache_env_var("gradle"), "LOCALRUNNER_CACHE_GRADLE");
    assert_eq!(cache_env_var("my-cache.1"), "LOCALRUNNER_CACHE_MY_CACHE_1");
}

#[test]
fn command_names_must_all_be_declared_by_the_runner() {
    let mut r = req(BASIC, Some("linux"));
    assert!(r.commands.is_empty(), "profiles never name commands");
    r.commands = set(&["build", "test"]);
    let mut caps = runner();
    caps.commands = set(&["build"]);
    assert_eq!(
        reasons(&caps, &r),
        [MismatchReason::CommandMissing {
            name: "test".into()
        }]
    );
    assert_eq!(
        MismatchReason::CommandMissing {
            name: "test".into()
        }
        .to_string(),
        "command test is not described"
    );
    caps.commands = set(&["build", "test", "lint"]);
    assert!(satisfies(&caps, &r).is_ok());
}

#[test]
fn a_record_with_the_old_max_zone_reads_as_the_zones_up_to_it() {
    let r = req(BASIC, None);
    let mut v = toml::Value::try_from(&r).unwrap();
    let o = v.as_table_mut().unwrap();
    o.remove("zones");
    o.insert("max_zone".into(), toml::Value::String("lan".into()));
    let back: Requirements = v.try_into().unwrap();
    assert_eq!(back.zones, Some(lr_core::Zone::up_to(lr_core::Zone::Lan)));
}
