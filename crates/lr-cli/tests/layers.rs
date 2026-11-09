use serde_json::Value;
use std::collections::BTreeSet;
use std::process::Command;

const ALLOWED: &[(&str, &[&str])] = &[
    ("lr-core", &[]),
    ("lr-abstractions", &["lr-core"]),
    ("lr-proto", &["lr-core"]),
    ("lr-net", &["lr-core", "lr-abstractions", "lr-proto"]),
    ("lr-store", &["lr-core", "lr-abstractions"]),
    ("lr-exec", &["lr-core", "lr-abstractions"]),
    ("lr-obs", &["lr-core", "lr-abstractions"]),
    ("lr-broker", &["lr-core", "lr-abstractions", "lr-proto"]),
    ("lr-hub", &["lr-core", "lr-abstractions", "lr-proto"]),
    ("lr-runner", &["lr-core", "lr-abstractions", "lr-proto"]),
    ("lr-dashboard", &["lr-core", "lr-abstractions", "lr-proto"]),
    (
        "lr-cli",
        &[
            "lr-core",
            "lr-abstractions",
            "lr-proto",
            "lr-net",
            "lr-store",
            "lr-exec",
            "lr-obs",
            "lr-broker",
            "lr-hub",
            "lr-runner",
            "lr-dashboard",
        ],
    ),
];

#[test]
fn dependency_rule_holds() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("cargo metadata must run");
    assert!(out.status.success(), "cargo metadata failed");
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata is json");
    let packages = meta["packages"].as_array().expect("packages array");

    let actual_names: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    let expected_names: BTreeSet<String> = ALLOWED.iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(actual_names, expected_names, "workspace crate set differs");

    for (name, allowed) in ALLOWED {
        let pkg = packages
            .iter()
            .find(|p| p["name"] == *name)
            .unwrap_or_else(|| panic!("crate {name} is missing"));
        let actual: BTreeSet<String> = pkg["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .filter(|n| expected_names.contains(n))
            .collect();
        let expected: BTreeSet<String> = allowed.iter().map(|s| s.to_string()).collect();
        assert_eq!(actual, expected, "workspace dependencies of {name} differ");
    }
}
