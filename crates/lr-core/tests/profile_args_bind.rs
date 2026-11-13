use lr_core::{bind_args, ArgSpec, ProfileError, StepSpec};

fn spec(name: &str) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        one_of: None,
        pattern: None,
        default: None,
        many: false,
        join: " ".into(),
    }
}

fn step(name: &str, command: &str) -> StepSpec {
    StepSpec {
        name: name.into(),
        command: command.into(),
        timeout_s: 60,
        mem_mb: None,
        cpu: None,
        tags: None,
        secrets: Vec::new(),
        continue_on_failure: false,
    }
}

fn given(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

fn bound(command: &str, specs: &[ArgSpec], pairs: &[(&str, &str)]) -> Result<String, String> {
    let mut steps = vec![step("run", command)];
    bind_args(&mut steps, specs, &given(pairs))
        .map(|()| steps.remove(0).command)
        .map_err(|e| match e {
            ProfileError::Args(text) => text,
            other => panic!("expected Args, got {other:?}"),
        })
}

#[test]
fn substitutes_value() {
    let out = bound("cargo test {target}", &[spec("target")], &[("target", "a")]);
    assert_eq!(out.unwrap(), "cargo test a");
}

#[test]
fn default_used_when_not_given() {
    let mut s = spec("target");
    s.default = Some("all".into());
    assert_eq!(
        bound("cargo test {target}", &[s], &[]).unwrap(),
        "cargo test all"
    );
}

#[test]
fn value_is_verbatim() {
    let out = bound("echo {x}", &[spec("x")], &[("x", "a b \"c\" $X; {x}")]);
    assert_eq!(out.unwrap(), "echo a b \"c\" $X; {x}");
}

#[test]
fn optional_segment_dropped_and_kept() {
    let mut target = spec("target");
    target.default = Some("all".into());
    let specs = [spec("pkg"), target];
    let cmd = "cargo test {? -p {pkg}} {target}";
    assert_eq!(bound(cmd, &specs, &[]).unwrap(), "cargo test all");
    assert_eq!(
        bound(cmd, &specs, &[("pkg", "x")]).unwrap(),
        "cargo test -p x all"
    );
}

#[test]
fn trailing_optional_segment_dropped_leaves_no_space() {
    let out = bound("cargo test {? -p {pkg}}", &[spec("pkg")], &[]);
    assert_eq!(out.unwrap(), "cargo test");
}

#[test]
fn segment_with_two_args_needs_both() {
    let specs = [spec("a"), spec("b")];
    let cmd = "run{? {a} {b}}";
    assert_eq!(bound(cmd, &specs, &[("a", "1")]).unwrap(), "run");
    assert_eq!(
        bound(cmd, &specs, &[("a", "1"), ("b", "2")]).unwrap(),
        "run1 2"
    );
}

#[test]
fn many_joins_values() {
    let mut s = spec("t");
    s.many = true;
    s.join = ",".into();
    let out = bound("x {t}", &[s], &[("t", "a"), ("t", "b")]);
    assert_eq!(out.unwrap(), "x a,b");
}

#[test]
fn repeated_name_without_many_is_error() {
    let err = bound("x {t}", &[spec("t")], &[("t", "a"), ("t", "b")]).unwrap_err();
    assert!(err.contains("`t`"), "{err}");
}

#[test]
fn missing_required_is_error() {
    let err = bound("x {t}", &[spec("t")], &[]).unwrap_err();
    assert!(err.contains("`t`") && err.contains("--arg"), "{err}");
}

#[test]
fn unknown_given_name_lists_declared() {
    let err = bound("x {t}", &[spec("t")], &[("u", "1"), ("t", "a")]).unwrap_err();
    assert!(err.contains("`u`") && err.contains("t"), "{err}");
    let none = bound("x", &[], &[("u", "1")]).unwrap_err();
    assert!(none.contains("`u`") && none.contains("none"), "{none}");
}

#[test]
fn value_must_match_one_of_and_pattern() {
    let mut list = spec("t");
    list.one_of = Some(vec!["a".into(), "b".into()]);
    let err = bound("x {t}", &[list], &[("t", "c")]).unwrap_err();
    assert!(err.contains("`c`") && err.contains("a, b"), "{err}");
    let mut re = spec("t");
    re.pattern = Some("[a-z]+".into());
    assert_eq!(
        bound("x {t}", &[re.clone()], &[("t", "ab")]).unwrap(),
        "x ab"
    );
    let err = bound("x {t}", &[re], &[("t", "ab1")]).unwrap_err();
    assert!(err.contains("[a-z]+"), "{err}");
}

#[test]
fn unknown_braces_untouched() {
    let out = bound(
        "git clone {repo_url} ${HOME} {a,b} {t}",
        &[spec("t")],
        &[("t", "v")],
    );
    assert_eq!(out.unwrap(), "git clone {repo_url} ${HOME} {a,b} v");
}

#[test]
fn no_specs_leaves_template_alone() {
    assert_eq!(
        bound("echo {? {x}} {y}", &[], &[]).unwrap(),
        "echo {? {x}} {y}"
    );
}

#[test]
fn applies_to_every_step() {
    let mut steps = vec![step("a", "one {t}"), step("b", "two {t}")];
    bind_args(&mut steps, &[spec("t")], &given(&[("t", "v")])).unwrap();
    assert_eq!(steps[0].command, "one v");
    assert_eq!(steps[1].command, "two v");
}
