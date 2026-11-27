//! The environment a systemd user unit gives its process: what `doctor` and `secret list`
//! compare an `env:` secret with for a runner that a unit starts, since the shell that runs
//! them is not the environment of the runner.

use std::collections::BTreeMap;
use std::path::Path;

/// The variables set by the unit `unit` under `home/.config/systemd/user`: its
/// `Environment=` lines and the `EnvironmentFile=` files (an optional file, written with a
/// leading `-`, may be missing), also from its `<unit>.d/*.conf` drop-ins, in the order systemd
/// reads them. `None` when the unit file does not exist. Values are kept only to tell an empty
/// variable from a set one and are never printed.
pub fn systemd_environment(home: &Path, unit: &str) -> Option<BTreeMap<String, String>> {
    let dir = home.join(".config/systemd/user");
    let main = std::fs::read_to_string(dir.join(unit)).ok()?;
    let mut texts = vec![main];
    let mut drop_ins: Vec<_> = std::fs::read_dir(dir.join(format!("{unit}.d")))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "conf"))
        .collect();
    drop_ins.sort();
    texts.extend(
        drop_ins
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok()),
    );
    let mut env = BTreeMap::new();
    for text in texts {
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("Environment=") {
                for word in words(rest) {
                    if let Some((k, v)) = word.split_once('=') {
                        env.insert(k.to_string(), v.to_string());
                    }
                }
            } else if let Some(rest) = line.strip_prefix("EnvironmentFile=") {
                let path = rest
                    .trim()
                    .trim_start_matches('-')
                    .replace("%h", &home.to_string_lossy());
                if let Ok(file) = std::fs::read_to_string(path) {
                    env.extend(file.lines().filter_map(assignment));
                }
            }
        }
    }
    Some(env)
}

/// [`systemd_environment`] of the unit that `autostart on` registers for runner `name`;
/// `None` off Linux, without a home directory or without that unit.
pub fn runner_unit_environment(name: &str) -> Option<BTreeMap<String, String>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let unit = crate::os_service::autostart::unit_for_argv(&[
        "runner".into(),
        "run".into(),
        "--name".into(),
        name.into(),
    ])?;
    systemd_environment(&crate::os_service::home_dir().ok()?, &unit)
}

fn assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    let (k, v) = line
        .strip_prefix("export ")
        .unwrap_or(line)
        .split_once('=')?;
    Some((k.trim().to_string(), unquote(v.trim()).to_string()))
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner;
        }
    }
    v
}

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}
