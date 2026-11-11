use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

fn skipped(line: &str) -> bool {
    line.starts_with("#[ignore") || line.starts_with("#[cfg(")
}

fn fn_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("async fn ")
        .or_else(|| line.strip_prefix("fn "))?;
    Some(rest.split('(').next().unwrap().to_string())
}

fn attributes_above(lines: &[&str], at: usize) -> Vec<String> {
    lines[..at]
        .iter()
        .rev()
        .take_while(|l| l.starts_with("#[") || l.starts_with("//"))
        .filter(|l| l.starts_with("#["))
        .map(|l| l.to_string())
        .collect()
}

fn check_attributes(attrs: &[String]) -> Result<(), String> {
    if !attrs.iter().any(|a| is_test_attribute(a)) {
        return Err("no test attribute".into());
    }
    match attrs.iter().find(|a| skipped(a)) {
        Some(a) => Err(format!("`{a}` keeps it from running")),
        None => Ok(()),
    }
}

fn runs_as_a_test(text: &str, name: &str) -> Result<(), String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|l| fn_name(l).as_deref() == Some(name))
        .ok_or_else(|| format!("no fn {name}"))?;
    check_attributes(&attributes_above(&lines, at))
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
        let Some(at) = (i + 1..lines.len()).find(|&j| fn_name(lines[j]).is_some()) else {
            errors.push(format!(
                "{}:{}: no test after `{line}`",
                file.display(),
                i + 1
            ));
            continue;
        };
        let name = fn_name(lines[at]).unwrap();
        if let Err(why) = check_attributes(&attributes_above(&lines, at)) {
            errors.push(format!("{}::{name}: {why}", file.display()));
            continue;
        }
        found
            .entry(id.trim().to_string())
            .or_default()
            .push(format!("{}::{name}", file.display()));
    }
    found
}

fn collect_tags(
    files: &[(String, String)],
    main: &str,
    errors: &mut Vec<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut all: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, text) in files {
        let tags = tags_of(Path::new(name), text, errors);
        if tags.is_empty() {
            continue;
        }
        let stem = name.trim_end_matches(".rs");
        let declared = format!("mod {stem};");
        if !main.lines().any(|l| l.trim() == declared) {
            errors.push(format!(
                "{name} has scenario tags but main.rs lacks `{declared}`, so its tests never run"
            ));
            continue;
        }
        for (id, tests) in tags {
            all.entry(id).or_default().extend(tests);
        }
    }
    all
}

fn e2e_tags(errors: &mut Vec<String>) -> BTreeMap<String, Vec<String>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("e2e");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        files.push((name, std::fs::read_to_string(&path).unwrap()));
    }
    let main = std::fs::read_to_string(dir.join("main.rs")).unwrap();
    collect_tags(&files, &main, errors)
}

fn covering_test_exists(reference: &str) -> Result<(), String> {
    let (file, name) = reference.split_once("::").unwrap();
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let text = std::fs::read_to_string(crates.join(file)).map_err(|e| e.to_string())?;
    runs_as_a_test(&text, name)
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
fn an_ignored_or_conditional_test_covers_nothing() {
    let mut errors = Vec::new();
    let text = "// scenario: 16.1\n#[tokio::test]\n#[ignore]\nasync fn later() {}\n\n\
                // scenario: 16.2\n#[test]\n#[cfg(unix)]\nfn elsewhere() {}\n";
    let tags = tags_of(Path::new("x.rs"), text, &mut errors);
    assert!(tags.is_empty(), "{tags:?}");
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(runs_as_a_test(text, "later").is_err());
    assert!(runs_as_a_test(text, "elsewhere").is_err());
    assert!(runs_as_a_test("#[test]\nfn plain() {}\n", "plain").is_ok());
    assert!(runs_as_a_test("fn helper() {}\n", "helper").is_err());
}

#[test]
fn tags_of_a_module_main_rs_does_not_declare_are_errors() {
    let mut errors = Vec::new();
    let files = [
        ("on.rs", "// scenario: 16.1\n#[test]\nfn a() {}\n"),
        ("off.rs", "// scenario: 16.2\n#[test]\nfn b() {}\n"),
    ]
    .map(|(n, t)| (n.to_string(), t.to_string()));
    let tags = collect_tags(&files, "mod common;\nmod on;\n", &mut errors);
    assert_eq!(tags.keys().cloned().collect::<Vec<_>>(), ["16.1"]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("mod off;"), "{errors:?}");
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
        if let Err(why) = covering_test_exists(test) {
            errors.push(format!(
                "{id}: the covering test {test} does not run: {why}"
            ));
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
