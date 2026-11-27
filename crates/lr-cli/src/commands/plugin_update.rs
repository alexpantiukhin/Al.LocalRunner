//! The `localrunner` plugin in the coding agents of this machine: `upgrade` finds the copies
//! that are older than the binary it installs and offers to update them through the CLI of the
//! agent (Claude Code, Codex).

use std::io::Write;
use std::path::PathBuf;

/// A coding agent that has a plugin CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code (`claude`).
    Claude,
    /// Codex (`codex`).
    Codex,
}

impl Agent {
    /// The program name.
    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
}

/// An installed copy of the `localrunner` plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The agent it is installed in.
    pub agent: Agent,
    /// `localrunner@<marketplace>`.
    pub id: String,
    /// The installed version.
    pub version: String,
    /// The installation scope, when the agent tells it (`user`, `project`, `local`).
    pub scope: Option<String>,
    /// The project of a project-scope copy.
    pub project: Option<PathBuf>,
}

impl Installed {
    fn marketplace(&self) -> &str {
        self.id.split_once('@').map_or("", |(_, m)| m)
    }
}

/// One program to run, optionally in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The directory it runs in.
    pub cwd: Option<PathBuf>,
}

impl Step {
    fn new(program: &str, args: &[&str], cwd: Option<PathBuf>) -> Step {
        Step {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd,
        }
    }

    /// The command as a person types it.
    pub fn line(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

/// Runs the programs of the agents.
pub trait Agents {
    /// Runs `step` and gives its standard output, or the reason it failed (the program is not
    /// installed, it exited with an error).
    fn run(&self, step: &Step) -> Result<String, String>;
}

/// The agents of this machine.
pub struct OsAgents;

impl Agents for OsAgents {
    fn run(&self, step: &Step) -> Result<String, String> {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", &step.program]);
            c
        } else {
            std::process::Command::new(&step.program)
        };
        cmd.args(&step.args).stdin(std::process::Stdio::null());
        if let Some(dir) = &step.cwd {
            cmd.current_dir(dir);
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let text = String::from_utf8_lossy(&out.stderr);
            let text = text.trim();
            Err(if text.is_empty() {
                format!("exit code {}", out.status.code().unwrap_or(-1))
            } else {
                text.lines().last().unwrap_or("").to_string()
            })
        }
    }
}

/// The `localrunner` plugins in `claude plugin list --json`.
pub fn parse_claude_list(json: &str) -> Vec<Installed> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?;
            id.starts_with("localrunner@").then(|| Installed {
                agent: Agent::Claude,
                id: id.to_string(),
                version: p.get("version").and_then(|v| v.as_str()).unwrap_or("").into(),
                scope: p.get("scope").and_then(|v| v.as_str()).map(String::from),
                project: p.get("projectPath").and_then(|v| v.as_str()).map(PathBuf::from),
            })
        })
        .collect()
}

/// The installed `localrunner` plugins in the table of `codex plugin list`.
pub fn parse_codex_list(text: &str) -> Vec<Installed> {
    text.lines()
        .filter(|l| l.starts_with("localrunner@"))
        .filter_map(|l| {
            let cols: Vec<&str> = l.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect();
            let (id, status, version) = (cols.first()?, cols.get(1)?, cols.get(2)?);
            status.starts_with("installed").then(|| Installed {
                agent: Agent::Codex,
                id: id.to_string(),
                version: version.to_string(),
                scope: None,
                project: None,
            })
        })
        .collect()
}

/// The copies in `found` older than `new` (a version such as `0.12.0`, a pre-release suffix is
/// ignored); a copy whose version cannot be read counts as older.
pub fn outdated(found: &[Installed], new: &str) -> Vec<Installed> {
    use super::upgrade_cmd::parse_version;
    let Some(new) = parse_version(new) else {
        return Vec::new();
    };
    found
        .iter()
        .filter(|p| parse_version(&p.version).is_none_or(|v| v.number < new.number))
        .cloned()
        .collect()
}

/// What updates `plugin`: the marketplace is refreshed first, so the agent sees the new
/// version, then the plugin is updated in its own scope (a project copy from its project).
pub fn update_steps(plugin: &Installed) -> Vec<Step> {
    let market = plugin.marketplace();
    match plugin.agent {
        Agent::Claude => {
            let scope = plugin.scope.as_deref().unwrap_or("user");
            vec![
                Step::new("claude", &["plugin", "marketplace", "update", market], plugin.project.clone()),
                Step::new(
                    "claude",
                    &["plugin", "update", &plugin.id, "--scope", scope, "--yes"],
                    plugin.project.clone(),
                ),
            ]
        }
        Agent::Codex => vec![
            Step::new("codex", &["plugin", "marketplace", "upgrade", market], None),
            Step::new("codex", &["plugin", "add", &plugin.id], None),
        ],
    }
}

/// The installed `localrunner` plugins of both agents; an agent that is not installed or does
/// not answer is skipped.
pub fn detect(agents: &dyn Agents) -> Vec<Installed> {
    let mut found = Vec::new();
    if let Ok(out) = agents.run(&Step::new("claude", &["plugin", "list", "--json"], None)) {
        found.extend(parse_claude_list(&out));
    }
    if let Ok(out) = agents.run(&Step::new("codex", &["plugin", "list"], None)) {
        found.extend(parse_codex_list(&out));
    }
    found
}

/// One line for the plan of `upgrade`.
pub fn describe(plugin: &Installed, new: &str) -> String {
    let place = match (&plugin.scope, &plugin.project) {
        (Some(s), Some(p)) => format!(", {s} scope, {}", p.display()),
        (Some(s), None) => format!(", {s} scope"),
        _ => String::new(),
    };
    format!(
        "plugin {} in {}: {} -> {new}{place}",
        plugin.id,
        plugin.agent.program(),
        plugin.version
    )
}

/// Updates every copy of `plugins`; one line per copy, `ok ...` or `warn ...` with the command
/// to repeat when a step failed.
pub fn run_updates(agents: &dyn Agents, plugins: &[Installed]) -> Vec<String> {
    plugins
        .iter()
        .map(|p| {
            for step in update_steps(p) {
                if let Err(e) = agents.run(&step) {
                    return format!(
                        "warn  plugin {} in {} was not updated: {e}; run: {}",
                        p.id,
                        p.agent.program(),
                        step.line()
                    );
                }
            }
            format!("ok    plugin {} in {} updated", p.id, p.agent.program())
        })
        .collect()
}

/// The plugin step of `upgrade`: the copies older than `new` are listed; with `dry_run` that is
/// all; otherwise the person confirms (`yes`, or `yes` typed) and they are updated. Without a
/// terminal and without `--yes`, or after a no, the lines say what is left and the commands to
/// run. Nothing is printed when no copy is older.
pub fn offer_updates(
    agents: &dyn Agents,
    new: &str,
    yes: bool,
    dry_run: bool,
    console: &mut super::confirm::Console<'_>,
) -> Vec<String> {
    let old = outdated(&detect(agents), new);
    if old.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = old.iter().map(|p| describe(p, new)).collect();
    if dry_run {
        return lines;
    }
    for l in &lines {
        let _ = writeln!(console.out, "{l}");
    }
    let go = super::confirm::confirm(
        "the plugins are older than the binary",
        "update the plugins now?",
        yes,
        console,
    );
    if matches!(go, Ok(true)) {
        return run_updates(agents, &old);
    }
    lines.clear();
    for p in &old {
        let commands: Vec<String> = update_steps(p).iter().map(Step::line).collect();
        lines.push(format!(
            "warn  plugin {} in {} is {}, the binary is {new}; update it: {}",
            p.id,
            p.agent.program(),
            p.version,
            commands.join(" && ")
        ));
    }
    lines
}
