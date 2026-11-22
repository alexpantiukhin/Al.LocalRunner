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

fn tools_tree(name: &str, bins: &[&str]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("lr-tools-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for bin in bins {
        std::fs::create_dir_all(root.join(bin)).unwrap();
    }
    root
}

#[test]
fn tools_dir_appends_its_bin_directories_to_path_in_name_order() {
    let root = tools_tree("order", &["node/bin", "jdk/bin", "bin", "docs/share"]);
    let dir = root.to_string_lossy().replace('\\', "/");
    let s = RunnerSettings::from_toml(
        &format!("name = \"a\"\n[env]\npath = \"/usr/bin\"\ntools_dir = \"{dir}\"\n"),
        "/s".into(),
    )
    .unwrap();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let parts: Vec<_> = s.path.as_deref().unwrap().split(sep).collect();
    let tail: Vec<_> = parts[1..]
        .iter()
        .map(|p| p.replace('\\', "/").replace(&dir, ""))
        .collect();
    assert_eq!(parts[0], "/usr/bin");
    assert_eq!(tail, ["/bin", "/jdk/bin", "/node/bin"], "{parts:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_tools_dir_leaves_path_as_written() {
    let s = parse("");
    assert_eq!(s.path.as_deref(), Some("/usr/bin"));
    let s = RunnerSettings::from_toml(
        "name = \"a\"\n[env]\npath = \"/usr/bin\"\ntools_dir = \"/no/such/dir\"\n",
        "/s".into(),
    )
    .unwrap();
    assert_eq!(s.path.as_deref(), Some("/usr/bin"));
}
