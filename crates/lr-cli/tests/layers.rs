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

const CORE: &[&str] = &[
    "lr-core",
    "lr-abstractions",
    "lr-proto",
    "lr-store",
    "lr-exec",
    "lr-broker",
    "lr-hub",
    "lr-runner",
];

const CONSUMER_WORDS: &[&str] = &[
    "cargo",
    "rustc",
    "typescript",
    "csharp",
    "deno",
    "npm",
    "yarn",
    "pnpm",
    "nodejs",
    "dotnet",
    "msbuild",
    "nuget",
    "gradle",
    "maven",
    "mvn",
    "python",
    "pip",
    "pytest",
    "java",
    "golang",
    "jest",
    "vitest",
    "mocha",
    "xunit",
    "nunit",
    "mstest",
    "junit",
    "playwright",
    "webpack",
    "cmake",
];

const AMBIGUOUS_WORDS: &[&str] = &["rust", "node", "go", "bun"];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

fn literals(line: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    let mut escaped = false;
    let mut prev = ' ';
    for c in line.chars() {
        if !inside && c == '/' && prev == '/' {
            break;
        }
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
                out.push(' ');
            } else {
                out.push(c);
            }
        } else if c == '"' && prev != '\'' {
            inside = true;
        }
        prev = c;
    }
    out
}

fn consumer_word(line: &str) -> Option<String> {
    words(line)
        .find(|w| CONSUMER_WORDS.contains(&w.as_str()))
        .or_else(|| words(&literals(line)).find(|w| AMBIGUOUS_WORDS.contains(&w.as_str())))
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_crates_name_no_consumer_language_toolchain_or_test_framework() {
    assert_eq!(
        consumer_word("Runs `cargo-nextest`; npm."),
        Some("cargo".into())
    );
    assert_eq!(
        consumer_word("let kind = \"go\"; // go on"),
        Some("go".into())
    );
    assert_eq!(consumer_word("// go on with the next node"), None);
    assert_eq!(consumer_word("fn node(&self) -> Category"), None);
    assert_eq!(consumer_word("x == \"Node-18\" || y"), Some("node".into()));
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    for name in CORE {
        let mut files = Vec::new();
        rust_files(&crates.join(name).join("src"), &mut files);
        assert!(!files.is_empty(), "{name} has no sources");
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                if let Some(w) = consumer_word(line) {
                    found.push(format!("{}:{}: {w}", file.display(), n + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "generic core names a consumer: {found:#?}"
    );
}
