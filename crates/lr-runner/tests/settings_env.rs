use lr_runner::RunnerSettings;

fn parse(extra: &str) -> RunnerSettings {
    let text = format!("name = \"a\"\n[env]\npath = \"/usr/bin\"\n{extra}");
    RunnerSettings::from_toml(&text, "/s".into()).unwrap()
}

#[test]
fn the_base_environment_is_path_home_temp_and_then_job_env() {
    let mut s = parse("[job_env]\nJAVA_HOME = \"/jdk\"\n");
    s.home = Some("/home/runner".into());
    s.temp = Some("/tmp/runner".into());
    let env = s.base_env();
    for pair in [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/runner"),
        ("TEMP", "/tmp/runner"),
        ("JAVA_HOME", "/jdk"),
    ] {
        assert!(env.contains(&(pair.0.into(), pair.1.into())), "{env:?}");
    }
}

#[test]
fn job_env_replaces_the_detected_home() {
    let mut s = parse("[job_env]\nHOME = \"/data/home\"\n");
    s.home = Some("/home/runner".into());
    let env = s.base_env();
    let homes: Vec<_> = env.iter().filter(|(n, _)| n == "HOME").collect();
    assert_eq!(homes, [&("HOME".to_string(), "/data/home".to_string())]);
}
