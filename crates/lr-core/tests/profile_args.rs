use lr_core::{parse_profiles, ArgSpec, Profile, ProfileError};

const BASE: &str = "[profile.t]\ncommand = \"cargo test {target}\"\ntimeout_s = 60\n";

fn one(text: &str) -> Profile {
    parse_profiles(text).unwrap().remove(0)
}

fn invalid(text: &str) -> (String, String) {
    match parse_profiles(text).unwrap_err() {
        ProfileError::Invalid { profile, field, .. } => (profile, field),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn with_arg(body: &str) -> String {
    format!("{BASE}[profile.t.args.target]\n{body}\n")
}

#[test]
fn args_table_parses() {
    let p = one(&with_arg("one_of = [\"all\", \"a\"]\ndefault = \"all\""));
    assert_eq!(
        p.args,
        vec![ArgSpec {
            name: "target".into(),
            one_of: Some(vec!["all".into(), "a".into()]),
            pattern: None,
            default: Some("all".into()),
            many: false,
            join: " ".into(),
        }]
    );
}

#[test]
fn many_and_join_parse() {
    let p = one(&with_arg("many = true\njoin = \",\""));
    assert!(p.args[0].many);
    assert_eq!(p.args[0].join, ",");
}

#[test]
fn profile_without_args_has_none() {
    assert!(one(BASE).args.is_empty());
}

#[test]
fn one_of_and_pattern_together_rejected() {
    let text = with_arg("one_of = [\"a\"]\npattern = \"a\"");
    assert_eq!(invalid(&text), ("t".into(), "args.target".into()));
}

#[test]
fn bad_regex_rejected() {
    let text = with_arg("pattern = \"(\"");
    assert_eq!(invalid(&text), ("t".into(), "args.target.pattern".into()));
}

#[test]
fn default_outside_one_of_rejected() {
    let text = with_arg("one_of = [\"a\"]\ndefault = \"b\"");
    assert_eq!(invalid(&text), ("t".into(), "args.target.default".into()));
}

#[test]
fn default_not_matching_pattern_rejected() {
    let text = with_arg("pattern = \"[a-z]+\"\ndefault = \"A1\"");
    assert_eq!(invalid(&text), ("t".into(), "args.target.default".into()));
}

#[test]
fn reserved_name_rejected() {
    let text = format!("{BASE}[profile.t.args.branch]\ndefault = \"x\"\n");
    assert_eq!(invalid(&text), ("t".into(), "args.branch".into()));
}

#[test]
fn unknown_key_in_arg_rejected() {
    let (profile, _) = invalid(&with_arg("colour = \"red\""));
    assert_eq!(profile, "t");
}

#[test]
fn join_without_many_rejected() {
    let text = with_arg("join = \",\"");
    assert_eq!(invalid(&text), ("t".into(), "args.target.join".into()));
}
