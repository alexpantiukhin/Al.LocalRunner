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

fn process(name: &str) -> Option<String> {
    match name {
        "ProgramFiles(x86)" => Some("C:\\Program Files (x86)".into()),
        "USERPROFILE" => Some("C:\\Users\\ci".into()),
        "JAVA_HOME" => Some("/from-process".into()),
        _ => None,
    }
}

#[test]
fn inherit_copies_listed_names_from_the_process_and_an_explicit_value_wins() {
    let s = parse(
        "[job_env]\ninherit = [\"ProgramFiles(x86)\", \"USERPROFILE\", \"JAVA_HOME\", \"NOT_SET\"]\njava_home = \"/jdk\"\n",
    );
    let env = s.owner_env_from(process);
    assert_eq!(env["ProgramFiles(x86)"], "C:\\Program Files (x86)");
    assert_eq!(env["USERPROFILE"], "C:\\Users\\ci");
    assert!(!env.contains_key("NOT_SET"), "{env:?}");
    assert_eq!(
        env.get("JAVA_HOME"),
        None,
        "the explicit name shadows the inherited one"
    );
    assert_eq!(env["java_home"], "/jdk");
    assert_eq!(s.inherited_missing(process), vec!["NOT_SET".to_string()]);
}

#[test]
fn nothing_but_the_listed_names_is_inherited() {
    let s = parse("[job_env]\ninherit = [\"USERPROFILE\"]\n");
    let env = s.owner_env_from(process);
    assert_eq!(env.keys().collect::<Vec<_>>(), vec!["USERPROFILE"]);
    assert!(parse("").owner_env_from(process).is_empty());
}

#[test]
fn inherit_refuses_reserved_names_bad_names_and_other_shapes() {
    for (body, reason) in [
        ("inherit = [\"PATH\"]", "reserved"),
        ("inherit = [\"temp\"]", "reserved"),
        ("inherit = [\"LOCALRUNNER_JOB_ID\"]", "reserved"),
        ("inherit = [\"Program*\"]", "not a valid variable name"),
        ("inherit = \"USERPROFILE\"", "must be a list"),
        ("inherit = [1]", "must be a list"),
        ("FOO = 1", "must be a string"),
    ] {
        let text = format!("name = \"a\"\n[env]\npath = \"/usr/bin\"\n[job_env]\n{body}\n");
        let err = RunnerSettings::from_toml(&text, "/s".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains(reason), "{body}: {err}");
    }
}
