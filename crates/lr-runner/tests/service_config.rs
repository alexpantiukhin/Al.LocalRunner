use lr_proto::WireServiceStatus;
use lr_runner::{Lifetime, Reuse, RunnerSettings, ServiceSpec};
use std::path::PathBuf;

fn parse(service: &str) -> Result<RunnerSettings, String> {
    let text = format!("name = \"r\"\n[service.build-daemon]\n{service}");
    RunnerSettings::from_toml(&text, PathBuf::from("/state")).map_err(|e| e.to_string())
}

fn spec(service: &str) -> ServiceSpec {
    parse(service).unwrap().services["build-daemon"].clone()
}

const FULL: &str = r#"
start = "daemon --serve"
ready_check = "daemon --status"
stop = "daemon --stop"
ready_timeout_s = 60
memory_mb = 2048
lifetime = "idle"
idle_timeout_s = 900
max_lifetime_s = 14400
reuse = "same_project"
restart = 2
stop_timeout_s = 5
"#;

#[test]
fn a_service_section_is_read_with_every_key() {
    let s = spec(FULL);
    assert_eq!(s.start, "daemon --serve");
    assert_eq!(s.ready_check, "daemon --status");
    assert_eq!(s.stop.as_deref(), Some("daemon --stop"));
    assert_eq!(s.ready_timeout_s, 60);
    assert_eq!(s.memory_mb, 2048);
    assert_eq!(s.lifetime, Lifetime::Idle);
    assert_eq!(s.idle_timeout_s, Some(900));
    assert_eq!(s.max_lifetime_s, 14400);
    assert_eq!(s.reuse, Reuse::SameProject);
    assert_eq!(s.restart, 2);
    assert_eq!(s.stop_timeout_s, 5);
    assert!(s.secrets.is_empty());
}

#[test]
fn defaults_are_same_client_reuse_and_bounded_restarts() {
    let s = spec("start = \"d\"\nready_check = \"c\"\nlifetime = \"keep\"\nmax_lifetime_s = 60\n");
    assert_eq!(s.reuse, Reuse::SameClient);
    assert_eq!(s.lifetime, Lifetime::Keep);
    assert_eq!(s.ready_timeout_s, 60);
    assert_eq!(s.stop_timeout_s, 10);
    assert_eq!(s.restart, 3);
    assert_eq!(s.memory_mb, 0);
    assert_eq!(s.stop, None);
}

#[test]
fn lifetime_defaults_to_per_job() {
    let s = spec("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\n");
    assert_eq!(s.lifetime, Lifetime::PerJob);
    assert_eq!(s.idle_timeout_s, None);
}

#[test]
fn max_lifetime_is_mandatory_for_every_lifetime() {
    for lifetime in ["per_job", "idle", "keep"] {
        let e = parse(&format!(
            "start = \"d\"\nready_check = \"c\"\nlifetime = \"{lifetime}\"\nidle_timeout_s = 5\n"
        ))
        .unwrap_err();
        assert!(e.contains("max_lifetime_s"), "{lifetime}: {e}");
    }
    let zero = parse("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 0\n").unwrap_err();
    assert!(zero.contains("max_lifetime_s"), "{zero}");
}

#[test]
fn readiness_check_and_start_are_mandatory() {
    let e = parse("start = \"d\"\nmax_lifetime_s = 60\n").unwrap_err();
    assert!(e.contains("ready_check"), "{e}");
    let e = parse("ready_check = \"c\"\nmax_lifetime_s = 60\n").unwrap_err();
    assert!(e.contains("start"), "{e}");
    let e = parse("start = \" \"\nready_check = \"c\"\nmax_lifetime_s = 60\n").unwrap_err();
    assert!(e.contains("start"), "{e}");
}

#[test]
fn idle_lifetime_needs_an_idle_timeout() {
    let e = parse("start = \"d\"\nready_check = \"c\"\nlifetime = \"idle\"\nmax_lifetime_s = 60\n")
        .unwrap_err();
    assert!(e.contains("idle_timeout_s"), "{e}");
}

#[test]
fn unknown_keys_and_values_are_refused() {
    let e = parse("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\nmax_lifetime = 5\n")
        .unwrap_err();
    assert!(e.contains("unknown field `max_lifetime`"), "{e}");
    let e = parse("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\nreuse = \"all\"\n")
        .unwrap_err();
    assert!(e.contains("all"), "{e}");
    let e =
        parse("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\nlifetime = \"forever\"\n")
            .unwrap_err();
    assert!(e.contains("forever"), "{e}");
}

#[test]
fn a_service_name_must_be_a_plain_word() {
    let text = "name = \"r\"\n[service.\"../x\"]\nstart = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\n";
    let e = RunnerSettings::from_toml(text, PathBuf::from("/s")).unwrap_err();
    assert!(e.to_string().contains("../x"), "{e}");
}

#[test]
fn a_service_with_a_secret_is_never_reused_wider_than_its_client() {
    let s = spec(
        "start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\nreuse = \"any\"\nsecrets = [\"registry_auth\"]\n",
    );
    assert_eq!(s.secrets, vec!["registry_auth".to_string()]);
    assert_eq!(s.reuse, Reuse::SameClient);
    let plain = spec("start = \"d\"\nready_check = \"c\"\nmax_lifetime_s = 60\nreuse = \"any\"\n");
    assert_eq!(plain.reuse, Reuse::Any);
}

#[test]
fn declared_services_are_offered_as_available() {
    let s = parse(FULL).unwrap();
    let caps = s.capabilities(None);
    assert_eq!(
        caps.services.get("build-daemon"),
        Some(&WireServiceStatus::Available)
    );
    assert_eq!(caps.service_mem_mb.get("build-daemon"), Some(&2048));
    let none = RunnerSettings::from_toml("name = \"r\"\n", PathBuf::from("/s")).unwrap();
    assert!(none.capabilities(None).services.is_empty());
}
