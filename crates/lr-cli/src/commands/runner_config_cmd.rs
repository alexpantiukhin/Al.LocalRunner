//! `runner config <name> [--diff]`: which `runner.toml` a runner reads and how its own copy
//! differs from the template.

use crate::build::runner_config_path;
use crate::paths::Layout;

fn own_path(layout: &Layout, name: &str) -> std::path::PathBuf {
    layout.runner_dir(name).join("runner.toml")
}

/// The `runner.toml` the runner `name` reads: its path, whether it is its own copy or the
/// template, and the file.
pub fn show(layout: &Layout, name: &str) -> Result<String, String> {
    crate::runner_config::check_runner_name(name)?;
    let path = runner_config_path(layout, name, None)
        .ok_or_else(|| format!("runner {name} has no runner.toml: run `localrunner init`"))?;
    let kind = if path == own_path(layout, name) {
        "its own copy; edits of the template do not reach it"
    } else {
        "the template; every runner without an own copy reads it"
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("# {}\n# {kind}\n{text}", path.display()))
}

/// The lines in which the own copy of runner `name` differs from the template (`-` only in the
/// template, `+` only in the copy).
pub fn diff(layout: &Layout, name: &str) -> Result<String, String> {
    crate::runner_config::check_runner_name(name)?;
    let own = own_path(layout, name);
    let template = layout.config_file("runner.toml");
    if !own.exists() {
        return Ok(format!(
            "runner {name} has no own copy: it reads the template {}",
            template.display()
        ));
    }
    let read = |p: &std::path::Path| {
        std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
    };
    let (mine, base) = (read(&own)?, read(&template)?);
    Ok(lines_diff(&base, &mine, &template, &own))
}

fn lines_diff(base: &str, mine: &str, template: &std::path::Path, own: &std::path::Path) -> String {
    let significant = |t: &str| -> Vec<String> {
        t.lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect()
    };
    let (base, mine) = (significant(base), significant(mine));
    let mut out = vec![
        format!("--- {}", template.display()),
        format!("+++ {}", own.display()),
    ];
    out.extend(
        base.iter()
            .filter(|l| !mine.contains(l))
            .map(|l| format!("-{l}")),
    );
    out.extend(
        mine.iter()
            .filter(|l| !base.contains(l))
            .map(|l| format!("+{l}")),
    );
    if out.len() == 2 {
        out.push("no differences".into());
    }
    out.join("\n")
}
