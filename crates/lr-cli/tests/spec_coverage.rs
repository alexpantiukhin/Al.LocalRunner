use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SPEC: &str = include_str!("../../../docs/architecture.html");
const SECTION: &str = "16";
const COUNT: u32 = 25;
const TAG: &str = "// scenario: ";

const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "16.5",
        "a real OS service needs administrator rights and changes the machine",
        "lr-cli/tests/service_rights.rs::broker_service_commands_without_administrator_rights_are_refused_before_any_change",
    ),
    (
        "16.7",
        "a second OS user cannot be created or logged in by a test",
        "lr-broker/tests/assign.rs::same_machine_order_goes_only_to_ipc_runner_of_the_same_os_user",
    ),
    (
        "16.9",
        "containers need Docker, which the test machines do not run",
        "lr-cli/tests/install/container.rs::compose_runs_broker_and_runner_from_the_versioned_image_as_non_root",
    ),
    (
        "16.25",
        "a real scheduler task or systemd unit changes the machine and starts at logon",
        "lr-cli/tests/setup.rs::autostart_of_one_named_runner_and_uninstall_take_off_only_their_entries",
    ),
];

fn scenarios(html: &str) -> Vec<u32> {
    let prefix = format!("<h4>{SECTION}.");
    html.match_indices(&prefix)
        .map(|(i, _)| {
            let rest = &html[i + prefix.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            assert!(
                rest[digits.len()..].starts_with(' '),
                "a scenario heading is `<h4>{SECTION}.N title</h4>`: {}",
                &rest[..rest.len().min(40)]
            );
            digits.parse().unwrap()
        })
        .collect()
}

fn is_test_attribute(line: &str) -> bool {
    line.starts_with("#[tokio::test") || line.starts_with("#[test]")
}

fn tags_of(file: &Path, text: &str, errors: &mut Vec<String>) -> BTreeMap<String, Vec<String>> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(id) = line.strip_prefix(TAG) else {
            continue;
        };
        let next = lines[i + 1..]
            .iter()
            .find(|l| !l.starts_with("//"))
            .copied()
            .unwrap_or("");
        if !is_test_attribute(next) {
            errors.push(format!(
                "{}:{}: `{line}` is not directly above a test attribute",
                file.display(),
                i + 1
            ));
            continue;
        }
        let name = lines[i + 1..]
            .iter()
            .find_map(|l| {
                let rest = l
                    .strip_prefix("async fn ")
                    .or_else(|| l.strip_prefix("fn "))?;
                Some(rest.split('(').next().unwrap().to_string())
            })
            .unwrap_or_default();
        found
            .entry(id.trim().to_string())
            .or_default()
            .push(format!("{}::{name}", file.display()));
    }
    found
}

fn e2e_tags(errors: &mut Vec<String>) -> BTreeMap<String, Vec<String>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("e2e");
    let mut all: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let short = PathBuf::from(path.file_name().unwrap());
        for (id, tests) in tags_of(&short, &text, errors) {
            all.entry(id).or_default().extend(tests);
        }
    }
    all
}

fn covering_test_exists(reference: &str) -> bool {
    let (file, name) = reference.split_once("::").unwrap();
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    std::fs::read_to_string(crates.join(file)).is_ok_and(|text| {
        text.lines().any(|l| {
            let l = l.trim();
            l.starts_with(&format!("fn {name}(")) || l.starts_with(&format!("async fn {name}("))
        })
    })
}

#[test]
fn the_spec_numbers_its_scenarios_from_one_to_twenty_five() {
    let found = scenarios(SPEC);
    let expected: Vec<u32> = (1..=COUNT).collect();
    assert_eq!(
        found, expected,
        "section {SECTION} changed: update the e2e tags and EXEMPT"
    );
}

#[test]
fn a_tag_is_found_only_directly_above_a_test() {
    let mut errors = Vec::new();
    let text = "// scenario: 16.1\nfn helper() {}\n\n// scenario: 16.2\n// scenario: 16.3\n\
                #[tokio::test(flavor = \"multi_thread\")]\nasync fn real() {}\n";
    let tags = tags_of(Path::new("x.rs"), text, &mut errors);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("x.rs:1"), "{errors:?}");
    assert_eq!(tags.keys().cloned().collect::<Vec<_>>(), ["16.2", "16.3"]);
    assert_eq!(tags["16.2"], ["x.rs::real"]);
}

#[test]
fn every_scenario_of_section_16_is_named_by_an_e2e_test() {
    let mut errors = Vec::new();
    let tagged = e2e_tags(&mut errors);
    let ordered: Vec<String> = scenarios(SPEC)
        .into_iter()
        .map(|n| format!("{SECTION}.{n}"))
        .collect();
    let ids: BTreeSet<String> = ordered.iter().cloned().collect();
    for id in tagged.keys() {
        if !ids.contains(id) {
            errors.push(format!("tag {id} names no scenario of section {SECTION}"));
        }
    }
    for (id, reason, test) in EXEMPT {
        assert!(!reason.is_empty(), "{id}: an exemption needs a reason");
        if !ids.contains(*id) {
            errors.push(format!("exemption {id} names no scenario"));
        }
        if tagged.contains_key(*id) {
            errors.push(format!("{id} is both tagged and exempt"));
        }
        if !covering_test_exists(test) {
            errors.push(format!("{id}: the covering test {test} does not exist"));
        }
    }
    let missing: Vec<&String> = ordered
        .iter()
        .filter(|id| !tagged.contains_key(*id) && !EXEMPT.iter().any(|(e, _, _)| e == id))
        .collect();
    if !missing.is_empty() {
        errors.push(format!(
            "scenarios without an e2e test or exemption: {missing:?}"
        ));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
