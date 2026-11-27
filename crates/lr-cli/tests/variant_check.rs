use lr_cli::commands::variant_check::{variant_warnings, RunnerFacts};
use lr_core::parse_profiles;
use std::collections::BTreeSet;

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn linux_runner() -> RunnerFacts {
    RunnerFacts {
        name: "r1".into(),
        platforms: set(&["linux"]),
        tags: set(&["cargo"]),
        programs: set(&["git"]),
    }
}

fn profile(variants: &str) -> lr_core::Profile {
    let text = format!("[profile.p]\ncommand = \"true\"\ntimeout_s = 60\n{variants}");
    parse_profiles(&text).unwrap().remove(0)
}

#[test]
fn a_variant_hidden_behind_an_earlier_wider_one_is_reported() {
    let p = profile(
        "[profile.p.variant.any]\ncommand = \"a\"\nwhen = { os = [\"linux\", \"macos\"] }\n\
         [profile.p.variant.linux]\ncommand = \"b\"\nwhen = { os = [\"linux\"] }\n",
    );
    let found = variant_warnings(&p, &[linux_runner()]);
    assert!(
        found
            .iter()
            .any(|w| w.contains("variant linux is never used") && w.contains("any")),
        "{found:?}"
    );
}

#[test]
fn variants_that_can_match_the_same_runner_are_reported_as_overlapping() {
    let p = profile(
        "[profile.p.variant.a-linux]\ncommand = \"a\"\nwhen = { os = [\"linux\"] }\n\
         [profile.p.variant.b-cargo]\ncommand = \"b\"\nwhen = { tags = [\"cargo\"] }\n",
    );
    let found = variant_warnings(&p, &[linux_runner()]);
    assert!(
        found
            .iter()
            .any(|w| w.contains("variants a-linux and b-cargo can match the same runner")),
        "{found:?}"
    );
}

#[test]
fn a_variant_no_runner_of_this_installation_matches_is_reported() {
    let p = profile("[profile.p.variant.mac]\ncommand = \"a\"\nwhen = { os = [\"macos\"] }\n");
    let found = variant_warnings(&p, &[linux_runner()]);
    assert!(
        found
            .iter()
            .any(|w| w.contains("variant mac: no runner of this installation matches")),
        "{found:?}"
    );
    assert!(
        variant_warnings(&p, &[]).is_empty(),
        "no runner known: nothing to compare with"
    );
}

#[test]
fn disjoint_variants_that_all_have_a_runner_give_no_warning() {
    let p = profile(
        "[profile.p.variant.linux]\ncommand = \"a\"\nwhen = { os = [\"linux\"] }\n\
         [profile.p.variant.windows]\ncommand = \"b\"\nwhen = { os = [\"windows\"], runner = [\"w1\"] }\n",
    );
    let windows = RunnerFacts {
        name: "w1".into(),
        platforms: set(&["windows"]),
        tags: BTreeSet::new(),
        programs: BTreeSet::new(),
    };
    assert!(variant_warnings(&p, &[linux_runner(), windows]).is_empty());
}
