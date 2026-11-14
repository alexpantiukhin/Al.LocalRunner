use lr_core::{parse_profiles, ProfileError, Requirements};

const BASE: &str = "[profile.t]\ncommand = \"true\"\ntimeout_s = 5\n";

fn labels(extra: &str) -> Vec<String> {
    parse_profiles(&format!("{BASE}{extra}"))
        .unwrap()
        .remove(0)
        .labels
}

#[test]
fn labels_are_sorted_and_unique() {
    assert_eq!(
        labels("labels = [\"work\", \"release\", \"work\"]\n"),
        vec!["release", "work"]
    );
}

#[test]
fn a_profile_without_labels_has_none() {
    assert!(labels("").is_empty());
    assert!(labels("labels = []\n").is_empty());
}

#[test]
fn an_empty_label_is_refused() {
    let e = parse_profiles(&format!("{BASE}labels = [\"work\", \" \"]\n")).unwrap_err();
    match e {
        ProfileError::Invalid { field, .. } => assert_eq!(field, "labels"),
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn labels_are_not_requirements() {
    let p = parse_profiles(&format!("{BASE}labels = [\"work\"]\ntags = [\"cargo\"]\n"))
        .unwrap()
        .remove(0);
    let r = Requirements::from_profile(&p, Some("linux"), None).unwrap();
    assert_eq!(r.tags.iter().collect::<Vec<_>>(), vec!["cargo"]);
    assert!(!format!("{r:?}").contains("work"));
}
