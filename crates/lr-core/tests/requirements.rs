use lr_core::{
    cache_env_var, parse_profiles, satisfies, Accept, Capabilities, MachineRef, MismatchReason,
    Profile, Requirements, ServiceStatus, Zone,
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
        zone: Zone::Lan,
        machine: None,
        accept: Accept::default(),
    }
}

fn req(text: &str, platform: Option<&str>) -> Requirements {
    Requirements::from_profile(&one(text), platform, Some(MachineRef("m1".into()))).unwrap()
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
    assert_eq!(r.max_zone, Some(Zone::Lan));
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
    assert!(Requirements::from_profile(&p, Some("ios"), None).is_err());
}

#[test]
fn no_platform_means_same_machine_as_client() {
    let r = req(BASIC, None);
    assert_eq!(r.platform, None);
    assert_eq!(r.same_machine, Some(MachineRef("m1".into())));
}

#[test]
fn platform_job_is_not_tied_to_a_machine() {
    assert_eq!(req(BASIC, Some("linux")).same_machine, None);
}

#[test]
fn same_machine_job_needs_runner_with_that_machine() {
    let r = req(BASIC, None);
    let mut caps = runner();
    assert_eq!(reasons(&caps, &r), [MismatchReason::NotSameMachine]);
    caps.machine = Some(MachineRef("other".into()));
    assert!(satisfies(&caps, &r).is_err());
    caps.machine = Some(MachineRef("m1".into()));
    assert!(satisfies(&caps, &r).is_ok());
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
        "нужен таймаут 7200 с, потолок раннера 3600 с"
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
    assert!(err.to_string().contains("adb не найден"));
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
    assert!(err.to_string().contains("нужно 12288 МБ"));
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
fn no_platform_and_no_client_machine_is_an_error() {
    let p = one(BASIC);
    assert!(Requirements::from_profile(&p, None, None).is_err());
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
    assert_eq!(
        reasons(&runner(), &r),
        [MismatchReason::Zone {
            runner: Zone::Lan,
            allowed: Zone::Local
        }]
    );
    let mut caps = runner();
    caps.zone = Zone::Local;
    assert!(satisfies(&caps, &r).is_ok());
}

#[test]
fn no_zone_limit_accepts_remote_runner() {
    let mut caps = runner();
    caps.zone = Zone::Remote;
    assert!(satisfies(&caps, &req(BASIC, Some("linux"))).is_ok());
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
