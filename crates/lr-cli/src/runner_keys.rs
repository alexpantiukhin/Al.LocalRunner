//! Keys of `runner.toml` that nothing reads. Two parsers read the file and each skips the keys
//! of the other, so neither can refuse a misspelled one; this lists them as warnings.

use toml::{Table, Value};

const TOP: [&str; 19] = [
    "name",
    "platforms",
    "tags",
    "mem_mb",
    "cpu",
    "allow_elevated",
    "allow_same_user_as_broker",
    "programs",
    "secret",
    "accept",
    "limits",
    "cache",
    "workspace",
    "grace",
    "env",
    "job_env",
    "cleanup",
    "service",
    "commands",
];

const RETENTION: [&str; 3] = ["max_size_gb", "retention_days", "evict"];

fn section_keys(section: &str) -> Option<&'static [&'static str]> {
    match section {
        "accept" => Some(&["min_mem_mb"]),
        "limits" => Some(&[
            "max_job_timeout_s",
            "enforce",
            "mem_headroom",
            "disk_headroom",
        ]),
        "env" => Some(&["path"]),
        "cache" | "workspace" => Some(&RETENTION),
        "grace" => Some(&["broker_lost_kill_s"]),
        _ => None,
    }
}

fn unknown_in(path: &str, table: &Table, known: &[&str], out: &mut Vec<String>) {
    for key in table.keys().filter(|k| !known.contains(&k.as_str())) {
        out.push(format!(
            "unknown key: `[{path}]` {key} (known: {})",
            known.join(", ")
        ));
    }
}

/// One line per key or section of `table` that `runner.toml` does not define.
pub fn unknown_keys(table: &Table) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in table {
        if !TOP.contains(&key.as_str()) {
            let what =
                if matches!(value, Value::Table(_) | Value::Array(_)) && !is_plain_list(value) {
                    "section"
                } else {
                    "key"
                };
            let shown = if what == "section" {
                format!("[{key}]")
            } else {
                key.clone()
            };
            out.push(format!(
                "unknown {what}: `{shown}` (known: {})",
                TOP.join(", ")
            ));
        }
    }
    for (name, known) in ["accept", "limits", "cache", "workspace", "grace", "env"]
        .iter()
        .filter_map(|s| section_keys(s).map(|k| (*s, k)))
    {
        if let Some(Value::Table(t)) = table.get(name) {
            unknown_in(name, t, known, &mut out);
        }
    }
    if let Some(Value::Table(secrets)) = table.get("secret") {
        for (secret, entry) in secrets {
            if let Value::Table(t) = entry {
                unknown_in(&format!("secret.{secret}"), t, &["from"], &mut out);
            }
        }
    }
    out
}

fn is_plain_list(value: &Value) -> bool {
    matches!(value, Value::Array(items) if !items.iter().all(Value::is_table))
}
