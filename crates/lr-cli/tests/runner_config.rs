use lr_broker::policy::HostResources;
use lr_cli::runner_config::{
    check_runner_name, elevation_allowed, instance_names, RunnerConfig, SecretFrom, MAX_COUNT,
};
use std::path::PathBuf;

const HOST: HostResources = HostResources {
    mem_mb: 32768,
    cpu: 12,
};

fn parse(text: &str, name: Option<&str>) -> RunnerConfig {
    RunnerConfig::parse(text, name, "host-pc", PathBuf::from("/state/r"), HOST).expect("parses")
}

#[test]
fn runner_without_declared_resources_takes_the_whole_host() {
    let c = parse("platforms = [\"linux\"]\n", None);
    assert_eq!((c.settings.mem_mb, c.settings.cpu), (32768, 12));
    assert_eq!(c.settings.name, "host-pc");
    assert!(c.settings.platforms.contains("linux"));
}

#[test]
fn declared_resources_are_what_the_runner_offers() {
    let c = parse("name = \"heavy\"\nmem_mb = 8192\ncpu = 4\n", None);
    assert_eq!((c.settings.mem_mb, c.settings.cpu), (8192, 4));
    assert_eq!(c.settings.name, "heavy");
    let partial = parse("cpu = 2\n", None);
    assert_eq!((partial.settings.mem_mb, partial.settings.cpu), (32768, 2));
}

#[test]
fn name_flag_beats_the_file_and_the_host() {
    assert_eq!(
        parse("name = \"file\"\n", Some("flag")).settings.name,
        "flag"
    );
    assert!(RunnerConfig::parse("", Some("bad:name"), "h", PathBuf::new(), HOST).is_err());
    assert!(RunnerConfig::parse("cpu = 0\n", None, "h", PathBuf::new(), HOST).is_err());
}

#[test]
fn count_makes_numbered_names() {
    assert_eq!(instance_names("box", 1).unwrap(), ["box"]);
    assert_eq!(
        instance_names("box", 3).unwrap(),
        ["box-1", "box-2", "box-3"]
    );
    assert!(instance_names("box", 0).is_err());
    assert_eq!(
        instance_names("box", MAX_COUNT).unwrap().len(),
        MAX_COUNT as usize
    );
    assert!(instance_names("box", MAX_COUNT + 1).is_err());
}

#[test]
fn elevation_and_secret_sources_are_read() {
    let c = parse(
        "allow_elevated = true\n[secret.registry_auth]\nfrom = \"env:REGISTRY_AUTH\"\n\
         [secret.signing]\nfrom = \"keychain:release\"\n",
        None,
    );
    assert!(c.allow_elevated);
    assert_eq!(
        c.secrets["registry_auth"],
        SecretFrom::Env("REGISTRY_AUTH".into())
    );
    assert_eq!(c.secrets["signing"], SecretFrom::Keychain("release".into()));
    assert!(!parse("", None).allow_elevated);
    let bad = "[secret.x]\nfrom = \"file:/etc/x\"\n";
    assert!(RunnerConfig::parse(bad, None, "h", PathBuf::new(), HOST).is_err());
}

#[test]
fn programs_that_fail_their_lookup_or_check_are_not_offered() {
    let git_check = if cfg!(windows) { "exit 0" } else { "true" };
    let text = format!(
        "[programs]\ngit = {{ check = \"{git_check}\" }}\n\
         surely-missing-program-xyz = {{}}\n\
         failing = {{ path = \"{}\", check = \"exit 3\" }}\n",
        std::env::current_exe()
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "\\\\")
    );
    let mut c = parse(&text, None);
    let problems = c.check_programs();
    assert!(c.settings.programs.contains("git"), "{problems:?}");
    assert!(!c.settings.programs.contains("surely-missing-program-xyz"));
    assert!(!c.settings.programs.contains("failing"));
    assert_eq!(problems.len(), 2, "{problems:?}");
}

#[test]
fn names_that_leave_the_runner_directory_are_refused() {
    for bad in [".", "..", "...", "a/b", "a\\b", ""] {
        assert!(check_runner_name(bad).is_err(), "{bad:?} accepted");
    }
    assert!(check_runner_name("box-1").is_ok());
    assert!(RunnerConfig::parse("", Some(".."), "h", PathBuf::new(), HOST).is_err());
}

#[test]
fn resources_above_the_host_or_an_unmeasured_host_are_refused() {
    let err = |text: &str, host: HostResources| {
        RunnerConfig::parse(text, None, "h", PathBuf::new(), host)
            .unwrap_err()
            .to_string()
    };
    assert!(err("mem_mb = 65536\n", HOST).contains("mem_mb"));
    assert!(err("cpu = 13\n", HOST).contains("cpu"));
    let unmeasured = HostResources { mem_mb: 0, cpu: 4 };
    assert!(err("", unmeasured).contains("mem_mb"));
    let declared = RunnerConfig::parse("mem_mb = 2048\n", None, "h", PathBuf::new(), unmeasured)
        .expect("a declared size needs no measurement");
    assert_eq!(declared.settings.mem_mb, 2048);
}

#[test]
fn elevated_start_needs_allow_elevated() {
    assert!(elevation_allowed(false, false).is_ok());
    assert!(elevation_allowed(true, true).is_ok());
    let refused = elevation_allowed(true, false).unwrap_err();
    assert!(refused.contains("allow_elevated"), "{refused}");
}
