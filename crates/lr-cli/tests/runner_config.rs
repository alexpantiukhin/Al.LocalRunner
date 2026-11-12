mod common;

use lr_broker::policy::HostResources;
use lr_cli::commands::join_cmd::with_flags;
use lr_cli::host::elevated_from;
use lr_cli::runner_config::{
    check_runner_name, elevation_allowed, instance_names, RunnerConfig, SecretFrom,
    DEFAULT_CHECK_TIMEOUT_S, MAX_COUNT,
};
use std::collections::BTreeSet;
use std::path::PathBuf;

const HOST: HostResources = HostResources {
    mem_mb: 32768,
    cpu: 12,
};

const ENV_PATH: &str = "[env]\npath = \"/usr/bin\"\n";

fn with_path(text: &str) -> String {
    if text.contains("[env]") {
        text.to_string()
    } else {
        format!("{text}\n{ENV_PATH}")
    }
}

fn parse(text: &str, name: Option<&str>) -> RunnerConfig {
    RunnerConfig::parse(
        &with_path(text),
        name,
        "host-pc",
        PathBuf::from("/state/r"),
        HOST,
    )
    .expect("parses")
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
    let bad = with_path("[secret.x]\nfrom = \"file:/etc/x\"\n");
    let e = RunnerConfig::parse(&bad, None, "h", PathBuf::new(), HOST).unwrap_err();
    assert!(e.to_string().contains("secret.x"), "{e}");
}

#[test]
fn programs_that_fail_their_lookup_or_check_are_not_offered() {
    let git_check = if cfg!(windows) { "exit 0" } else { "true" };
    let text = format!(
        "[programs]\ngit = {{ check = \"{git_check}\" }}\n\
         surely-missing-program-xyz = {{}}\n\
         failing = {{ path = \"{}\", check = \"exit 3\" }}\n{}",
        std::env::current_exe()
            .unwrap()
            .display()
            .to_string()
            .replace('\\', "\\\\"),
        common::env_path_toml()
    );
    let mut c = parse(&text, None);
    let problems = c.check_programs();
    assert!(c.settings.programs.contains("git"), "{problems:?}");
    assert!(!c.settings.programs.contains("surely-missing-program-xyz"));
    assert!(!c.settings.programs.contains("failing"));
    assert_eq!(problems.len(), 2, "{problems:?}");
}

#[test]
fn a_check_that_outlives_its_timeout_is_stopped_and_the_program_is_not_offered() {
    assert_eq!(DEFAULT_CHECK_TIMEOUT_S, 10);
    let slow = if cfg!(windows) {
        r"%SystemRoot%\System32\PING.EXE -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let text = format!(
        "[programs]\nslow = {{ path = '{}', check = '{slow}', check_timeout_s = 1 }}\n{}",
        std::env::current_exe().unwrap().display(),
        common::env_path_toml()
    );
    let mut c = parse(&text, None);
    let started = std::time::Instant::now();
    let problems = c.check_programs();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(
        problems,
        [format!("program slow: check `{slow}` took longer than 1s")]
    );
    assert!(c.settings.programs.is_empty());
}

#[test]
fn a_check_runs_with_the_variables_of_the_env_section() {
    let probe = if cfg!(windows) {
        r#"if "%CHK_VAR%"=="yes" (exit 0) else (exit 1)"#
    } else {
        r#"test "$CHK_VAR" = yes"#
    };
    let programs = format!(
        "[programs]\nprobe = {{ path = '{}', check = '{probe}' }}\n",
        std::env::current_exe().unwrap().display()
    );
    let mut with = parse(
        &format!("{programs}{}CHK_VAR = \"yes\"\n", common::env_path_toml()),
        None,
    );
    let problems = with.check_programs();
    assert!(with.settings.programs.contains("probe"), "{problems:?}");
    let mut without = parse(&format!("{programs}{}", common::env_path_toml()), None);
    assert_eq!(without.check_programs().len(), 1);
    assert!(without.settings.programs.is_empty());
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
fn names_windows_cannot_hold_as_a_directory_are_refused() {
    for bad in [
        "CON", "con", "NUL", "nul.txt", "Con.txt", "PRN", "AUX", "COM1", "com9", "LPT1",
        "lpt9.log", "COM¹", "CONIN$", "box.", "box ", "box..",
    ] {
        assert!(check_runner_name(bad).is_err(), "{bad:?} accepted");
    }
    for good in ["console", "com10", "nullable", "box.1", "a.b"] {
        assert!(check_runner_name(good).is_ok(), "{good:?} refused");
    }
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
    let text = with_path("mem_mb = 2048\n");
    let declared = RunnerConfig::parse(&text, None, "h", PathBuf::new(), unmeasured)
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

#[test]
fn runner_without_env_path_does_not_start() {
    let e = RunnerConfig::parse("[programs]\ngit = {}\n", None, "h", PathBuf::new(), HOST)
        .unwrap_err()
        .to_string();
    assert!(e.contains("[env] path") && e.contains("13.4"), "{e}");
    for blank in ["", "   "] {
        let text = format!("[env]\npath = \"{blank}\"\n");
        let e = RunnerConfig::parse(&text, None, "h", PathBuf::new(), HOST)
            .unwrap_err()
            .to_string();
        assert!(e.contains("[env] path") && e.contains("13.4"), "{e}");
    }
    let set = parse("[env]\npath = \"/opt/tools/bin\"\n", None);
    assert_eq!(set.settings.path.as_deref(), Some("/opt/tools/bin"));
}

#[test]
fn platforms_and_tags_flags_override_the_file() {
    let file =
        "name = \"box\"\nplatforms = [\"linux\"]\ntags = [\"gpu\"]\n\n[env]\npath = \"/opt/bin\"\n";
    let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let text = with_flags(file.into(), &list(&["windows", "android"]), &list(&["emu"])).unwrap();
    let c = parse(&text, None);
    let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(c.settings.platforms, set(&["android", "windows"]));
    assert_eq!(c.settings.tags, set(&["emu"]));
    assert_eq!(c.settings.name, "box");
    assert_eq!(c.settings.path.as_deref(), Some("/opt/bin"));
    let only_tags = parse(
        &with_flags(file.into(), &[], &list(&["emu"])).unwrap(),
        None,
    );
    assert_eq!(
        only_tags.settings.platforms,
        set(&["linux"]),
        "no --platforms keeps the file's"
    );
    let untouched = parse(&with_flags(file.into(), &[], &[]).unwrap(), None);
    assert_eq!(
        (untouched.settings.platforms, untouched.settings.tags),
        (set(&["linux"]), set(&["gpu"]))
    );
}

#[test]
fn undetermined_rights_count_as_elevated() {
    assert!(
        elevated_from(None, Some("S-1-5-21-1-2-3-1001")),
        "token unreadable"
    );
    assert!(elevated_from(Some(false), None), "user unreadable");
    assert!(elevated_from(None, None));
    assert!(elevated_from(Some(false), Some("S-1-5-18")), "SYSTEM");
    assert!(elevated_from(Some(true), Some("S-1-5-21-1-2-3-1001")));
    assert!(!elevated_from(Some(false), Some("S-1-5-21-1-2-3-1001")));
    let refused = elevation_allowed(elevated_from(None, None), false).unwrap_err();
    assert!(refused.contains("allow_elevated"), "{refused}");
}

#[test]
fn programs_are_looked_up_in_env_path_not_in_the_process_path() {
    assert!(
        lr_cli::runner_config::find_in_path("git", &std::env::var("PATH").unwrap()).is_some(),
        "the test process finds git on its own PATH"
    );
    let dir = common::TempDir::new("env-path-only");
    let text = format!(
        "[programs]\ngit = {{}}\n\n[env]\npath = {}\n",
        toml::Value::String(dir.path().display().to_string())
    );
    let mut c = parse(&text, None);
    let problems = c.check_programs();
    assert!(
        !c.settings.programs.contains("git"),
        "git is not in [env] path, so jobs could not run it"
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("program git:"), "{problems:?}");
}

#[test]
fn allow_same_user_as_broker_is_false_by_default_and_must_be_a_boolean() {
    assert!(!parse("", None).allow_same_user_as_broker);
    assert!(parse("allow_same_user_as_broker = true\n", None).allow_same_user_as_broker);
    assert!(lr_cli::runner_config::allows_same_user_as_broker(
        "allow_same_user_as_broker = true\n"
    ));
    assert!(!lr_cli::runner_config::allows_same_user_as_broker(
        "allow_same_user_as_broker = \"yes\"\n"
    ));
    let typo = RunnerConfig::parse(
        &with_path("allow_same_user_as_broker = \"yes\"\n"),
        None,
        "host-pc",
        PathBuf::from("/state/r"),
        HOST,
    );
    assert!(typo.is_err(), "{typo:?}");
}
