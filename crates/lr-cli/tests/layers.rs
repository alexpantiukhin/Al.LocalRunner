use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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

const HTTP_CLIENTS: &[&str] = &[
    "reqwest",
    "ureq",
    "isahc",
    "surf",
    "curl",
    "attohttpc",
    "minreq",
    "awc",
    "ehttp",
    "http-req",
    "hyper-tls",
    "hyper-rustls",
    "hyper-openssl",
    "opentelemetry-http",
    "opentelemetry-otlp",
];

const CLIENT_FEATURES: &[(&str, &[&str])] = &[
    ("hyper", &["client"]),
    ("hyper-util", &["client", "client-legacy"]),
];

fn root_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn table(path: &std::path::Path) -> toml::Table {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn normal_deps(manifest: &toml::Table) -> Vec<(String, &toml::Value)> {
    let mut sections: Vec<&toml::Table> = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .collect();
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        sections.extend(
            targets
                .values()
                .filter_map(|t| t.get("dependencies").and_then(toml::Value::as_table)),
        );
    }
    sections
        .into_iter()
        .flat_map(|t| t.iter())
        .map(|(key, spec)| {
            let name = spec
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            (name.to_string(), spec)
        })
        .collect()
}

fn features<'a>(spec: &'a toml::Value, workspace: &'a toml::Table, name: &str) -> Vec<&'a str> {
    let own = spec.get("features").and_then(toml::Value::as_array);
    let shared = workspace
        .get(name)
        .and_then(|w| w.get("features"))
        .and_then(toml::Value::as_array);
    own.into_iter()
        .chain(shared)
        .flatten()
        .filter_map(toml::Value::as_str)
        .collect()
}

fn lock_dependencies() -> BTreeMap<String, BTreeSet<String>> {
    let lock = table(&root_dir().join("Cargo.lock"));
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in lock["package"].as_array().unwrap() {
        let deps = out
            .entry(p["name"].as_str().unwrap().to_string())
            .or_default();
        for d in p
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let d = d.as_str().unwrap();
            deps.insert(d.split(' ').next().unwrap().to_string());
        }
    }
    out
}

fn http_clients_of(root: &str, lock: &BTreeMap<String, BTreeSet<String>>) -> (usize, Vec<String>) {
    let crates = root_dir().join("crates");
    let workspace_toml = table(&root_dir().join("Cargo.toml"));
    let workspace = workspace_toml["workspace"]["dependencies"]
        .as_table()
        .unwrap();
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    let mut todo = vec![root.to_string()];
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if HTTP_CLIENTS.contains(&name.as_str()) {
            found.push(format!("{root}: {name}"));
        }
        let manifest = crates.join(&name).join("Cargo.toml");
        if manifest.is_file() {
            let manifest = table(&manifest);
            for (dep, spec) in normal_deps(&manifest) {
                let enabled = features(spec, workspace, &dep);
                for (krate, client) in CLIENT_FEATURES {
                    if dep == *krate && client.iter().any(|c| enabled.contains(c)) {
                        found.push(format!("{root}: {name} enables {dep} {enabled:?}"));
                    }
                }
                todo.push(dep);
            }
        } else {
            todo.extend(lock.get(&name).into_iter().flatten().cloned());
        }
    }
    (seen.len(), found)
}

#[test]
fn nothing_leaves_the_machine_hub_cli_and_dashboard_carry_no_http_client() {
    let lock = lock_dependencies();
    let mut found = Vec::new();
    for root in ["lr-hub", "lr-cli", "lr-dashboard"] {
        let (reached, clients) = http_clients_of(root, &lock);
        assert!(reached > 3, "{root}");
        found.extend(clients);
    }
    assert!(found.is_empty(), "an HTTP client: {found:#?}");
}

fn first_arguments<'a>(script: &'a str, call: &str) -> Vec<&'a str> {
    script
        .match_indices(call)
        .filter(|(i, _)| {
            !script[..*i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                || call.starts_with('.')
        })
        .map(|(i, _)| {
            let rest = script[i + call.len()..].trim_start();
            let end = rest.find([',', ')']).unwrap_or(rest.len());
            rest[..end].trim()
        })
        .collect()
}

fn same_origin(argument: &str) -> bool {
    argument.starts_with("\"/") && !argument.starts_with("\"//")
}

#[test]
fn nothing_leaves_the_machine_the_page_requests_only_its_own_origin() {
    assert!(same_origin("\"/api/snapshot\""));
    assert!(!same_origin("\"//evil.example/x\""));
    assert!(!same_origin("\"https://evil.example/x\""));
    let script = include_str!("../../lr-dashboard/src/page.js");
    let fetched = first_arguments(script, "fetch(");
    assert!(fetched.len() >= 3, "{fetched:?}");
    let posted = first_arguments(script, "post(");
    let streamed = first_arguments(script, "EventSource(");
    assert!(!streamed.is_empty());
    for a in fetched.iter().chain(&posted).chain(&streamed) {
        assert!(
            same_origin(a) || *a == "path",
            "a request leaves the page's origin: {a}"
        );
    }
    assert!(
        posted.iter().filter(|a| **a != "path").count() >= 2,
        "{posted:?}"
    );
    for banned in ["XMLHttpRequest", "sendBeacon", "WebSocket(", "import("] {
        assert!(!script.contains(banned), "{banned}");
    }
}
