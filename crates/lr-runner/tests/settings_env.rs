use lr_runner::RunnerSettings;

fn parse(env: &str) -> Result<RunnerSettings, String> {
    let text = format!("name = \"a\"\n[env]\npath = \"/usr/bin\"\n{env}");
    RunnerSettings::from_toml(&text, "/s".into()).map_err(|e| e.to_string())
}

#[test]
fn env_section_keeps_other_variables_next_to_path() {
    let s =
        parse("JAVA_HOME = \"/jdk\"\nCARGO_HOME = \"/cargo\"\nHOME = \"/data/home\"\n").unwrap();
    assert_eq!(s.path.as_deref(), Some("/usr/bin"));
    assert_eq!(s.env.len(), 3);
    assert_eq!(s.env["JAVA_HOME"], "/jdk");
    assert_eq!(s.env["HOME"], "/data/home");
}

#[test]
fn env_section_without_extras_has_none() {
    assert!(parse("").unwrap().env.is_empty());
}

#[test]
fn env_section_refuses_reserved_and_malformed_variables() {
    for bad in [
        "PATH = \"/x\"\n",
        "Path = \"/x\"\n",
        "LOCALRUNNER_JOB_ID = \"x\"\n",
        "JAVA_HOME = 17\n",
        "\"A=B\" = \"x\"\n",
        "\"\" = \"x\"\n",
    ] {
        let e = parse(bad).unwrap_err();
        assert!(e.contains("[env]") || e.contains("expected"), "{bad}: {e}");
    }
}
