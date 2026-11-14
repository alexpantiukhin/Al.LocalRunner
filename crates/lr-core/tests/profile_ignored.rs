use lr_core::{ignored_sections, parse_profiles};

const TYPO: &str = "[profiles.x]\ncommand = \"true\"\ntimeout_s = 5\n";

#[test]
fn a_typo_section_is_reported_and_parsing_still_ignores_it() {
    assert_eq!(
        ignored_sections(TYPO).unwrap(),
        vec!["profiles".to_string()]
    );
    assert!(parse_profiles(TYPO).unwrap().is_empty());
}

#[test]
fn known_sections_and_top_level_scalars() {
    let text = "[profile.a]\ncommand = \"true\"\ntimeout_s = 5\n[needs.n]\ntimeout_s = 5\n";
    assert!(ignored_sections(text).unwrap().is_empty());
    let text = format!("version = 1\n[zzz]\n[aaa]\n{text}");
    assert_eq!(ignored_sections(&text).unwrap(), ["aaa", "version", "zzz"]);
}

#[test]
fn invalid_toml_is_a_syntax_error() {
    assert!(ignored_sections("[profile").is_err());
}
