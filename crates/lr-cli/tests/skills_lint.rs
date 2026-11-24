use clap::CommandFactory;
use lr_cli::commands::Cli;
use std::path::PathBuf;

fn plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}

const SKILLS: [&str; 8] = [
    "autostart-runner",
    "down-runner",
    "install-runner",
    "repo-setup",
    "run",
    "status-runner",
    "up-runner",
    "upgrade-runner",
];

fn skill(name: &str) -> String {
    let path = plugin().join("skills").join(name).join("SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn command_spans(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("localrunner "))
        .map(str::to_string)
        .collect()
}

fn has_flag(cmd: &clap::Command, flag: &str) -> bool {
    cmd.get_arguments().any(|a| {
        flag.strip_prefix("--")
            .is_some_and(|long| a.get_long() == Some(long))
            || flag
                .strip_prefix('-')
                .filter(|s| s.len() == 1)
                .and_then(|s| s.chars().next())
                .is_some_and(|c| a.get_short() == Some(c))
    })
}

fn check_span(span: &str) -> Result<(), String> {
    let mut root = Cli::command().version("as main sets it");
    root.build();
    let mut cmd = &root;
    let mut path = Vec::new();
    let words: Vec<&str> = span.split_whitespace().skip(1).collect();
    let mut rest = words.as_slice();
    let mut globals = 0;
    while let Some(arg) = rest
        .first()
        .and_then(|w| w.strip_prefix("--"))
        .and_then(|long| {
            root.get_arguments()
                .find(|a| a.is_global_set() && a.get_long() == Some(long))
        })
    {
        let width = if arg.get_action().takes_values() {
            2
        } else {
            1
        };
        rest = &rest[width.min(rest.len())..];
        globals += 1;
    }
    while let Some((word, tail)) = rest.split_first() {
        match cmd.find_subcommand(word) {
            Some(sub) => {
                path.push(*word);
                cmd = sub;
                rest = tail;
            }
            None => break,
        }
    }
    let only_globals = globals > 0 && rest.is_empty();
    if path.is_empty() && !only_globals && !rest.first().is_some_and(|w| w.starts_with('-')) {
        return Err(format!("`{span}`: no such command"));
    }
    for token in rest {
        for part in token.trim_matches(|c| c == '[' || c == ']').split('|') {
            if part.starts_with('-') && !has_flag(cmd, part) && !has_flag(&root, part) {
                return Err(format!("`{span}`: {} has no {part}", path.join(" ")));
            }
        }
    }
    Ok(())
}

#[test]
fn every_skill_names_real_commands_and_reads_the_os_files() {
    let mut found: Vec<String> = std::fs::read_dir(plugin().join("skills"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    assert_eq!(found, SKILLS, "the skills of spec 11.2");
    for os in ["windows", "linux", "macos"] {
        let path = plugin().join("shared").join("os").join(format!("{os}.md"));
        assert!(path.is_file(), "{}", path.display());
    }
    for name in SKILLS {
        let text = skill(name);
        let head = text
            .strip_prefix("---\n")
            .and_then(|t| t.split_once("\n---\n"))
            .map(|(h, _)| h)
            .unwrap_or_else(|| panic!("{name}: frontmatter between --- lines"));
        assert!(
            head.lines().any(|l| l == format!("name: {name}")),
            "{name}: name in frontmatter"
        );
        assert!(
            head.lines().any(|l| l.starts_with("description: ")),
            "{name}: description in frontmatter"
        );
        let spans = command_spans(&text);
        assert!(!spans.is_empty(), "{name} names no command");
        for span in spans {
            check_span(&span).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    for name in [
        "install-runner",
        "up-runner",
        "down-runner",
        "autostart-runner",
        "upgrade-runner",
    ] {
        assert!(
            skill(name).contains("`../../shared/os/<windows|linux|macos>.md`"),
            "{name} reads the OS file"
        );
    }
    let install = skill("install-runner");
    assert!(
        install.contains("never store, log or echo it"),
        "the registration token is never stored"
    );
}

#[test]
fn the_span_check_refuses_unknown_commands_and_flags() {
    assert!(check_span("localrunner status --json").is_ok());
    assert!(check_span("localrunner broker add").is_ok());
    assert!(check_span("localrunner launch").is_err());
    assert!(check_span("localrunner status --colour").is_err());
    assert!(check_span("localrunner down [--drain|--purge]").is_err());
    assert!(check_span(
        "localrunner --state-dir <dir> --config-dir <dir> config set runner:<name> k v --string"
    )
    .is_ok());
    assert!(
        check_span("localrunner --config-dir <dir> config set runner:<name> k v --colour").is_err()
    );
    assert!(check_span("localrunner --config-dir <dir> launch").is_err());
}

#[test]
fn every_plugin_text_names_real_commands() {
    for (path, text) in plugin_texts() {
        for span in command_spans(&text) {
            check_span(&span).unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }
}

#[test]
fn repo_setup_keeps_its_scope() {
    let text = skill("repo-setup")
        + &std::fs::read_to_string(plugin().join("skills/repo-setup/reference.md")).unwrap();
    for word in [
        "GitHub",
        "workflow",
        "CI ",
        "AGENTS.md",
        "CLAUDE.md",
        "docs/runners.md",
        "plugin install",
        "allow_elevated = true",
    ] {
        assert!(
            !text.contains(word),
            "{word} is out of scope for repo-setup"
        );
    }
    for cmd in [
        "`localrunner profile check`",
        "`localrunner profile check --json`",
        "`localrunner profile check --file <path>`",
        "`localrunner doctor`",
        "`localrunner policy check`",
        "`localrunner runners`",
    ] {
        assert!(text.contains(cmd), "{cmd}");
    }
}

const CONFIG_FILES: [&str; 4] = [
    "runner.toml",
    "policy.toml",
    "hub-policy.toml",
    "logging.toml",
];

const HAND_EDITS: [&str; 13] = [
    "edit",
    "add",
    "set",
    "put",
    "change",
    "append",
    "uncomment",
    "insert",
    "remove",
    "write",
    "apply",
    "declare",
    "list",
];

fn changes_through_the_cli(code: &str) -> bool {
    let words: Vec<&str> = code.split_whitespace().collect();
    words.first() == Some(&"localrunner")
        && words.windows(2).any(|w| match w {
            ["config", verb] => ["set", "unset"].contains(verb),
            ["policy", verb] => !["check", "reload"].contains(verb),
            _ => false,
        })
}

enum Token<'a> {
    Word(String),
    Code(&'a str),
}

fn units(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            out.extend(current.take());
            continue;
        }
        let starts = ["- ", "* ", "|", "#", "```"]
            .iter()
            .any(|p| line.starts_with(p))
            || line
                .split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        match current.as_mut() {
            Some(unit) if !starts => {
                unit.push(' ');
                unit.push_str(line);
            }
            _ => {
                out.extend(current.take());
                current = Some(line.to_string());
            }
        }
    }
    out.extend(current);
    out
}

fn tokens(clause: &str) -> Vec<Token<'_>> {
    clause
        .split('`')
        .enumerate()
        .flat_map(|(i, part)| match i % 2 {
            1 => vec![Token::Code(part)],
            _ => part
                .to_lowercase()
                .split(|c: char| !c.is_ascii_alphabetic())
                .filter(|w| !w.is_empty())
                .map(|w| Token::Word(w.to_string()))
                .collect(),
        })
        .collect()
}

fn is_hand_edit(clause: &str) -> bool {
    let tokens = tokens(clause);
    let unbound_verb = tokens.iter().enumerate().any(|(i, token)| {
        let Token::Word(word) = token else {
            return false;
        };
        let negated =
            i > 0 && matches!(&tokens[i - 1], Token::Word(w) if w == "never" || w == "not");
        let through_cli = tokens[i + 1..]
            .iter()
            .find_map(|t| match t {
                Token::Code(code) => Some(*code),
                Token::Word(_) => None,
            })
            .is_some_and(changes_through_the_cli);
        HAND_EDITS.contains(&word.as_str()) && !negated && !through_cli
    });
    let assignment = tokens.iter().any(
        |t| matches!(t, Token::Code(code) if !changes_through_the_cli(code) && code.contains(" = ")),
    );
    unbound_verb || assignment
}

fn hand_edits(text: &str) -> Vec<String> {
    units(text)
        .iter()
        .flat_map(|unit| {
            unit.split(". ")
                .flat_map(|s| s.split("; "))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|c| CONFIG_FILES.iter().any(|f| c.contains(f)))
        .filter(|c| is_hand_edit(c))
        .collect()
}

fn plugin_texts() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = SKILLS
        .iter()
        .map(|n| (format!("skills/{n}/SKILL.md"), skill(n)))
        .collect();
    for rel in [
        "skills/repo-setup/reference.md",
        "shared/os/windows.md",
        "shared/os/linux.md",
        "shared/os/macos.md",
    ] {
        let path = plugin().join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .replace("\r\n", "\n");
        out.push((rel.to_string(), text));
    }
    out
}

#[test]
fn no_skill_edits_a_config_file_by_hand() {
    let found: Vec<String> = plugin_texts()
        .into_iter()
        .flat_map(|(path, text)| {
            hand_edits(&text)
                .into_iter()
                .map(move |c| format!("{path}: {c}"))
        })
        .collect();
    assert!(
        found.is_empty(),
        "change these through `localrunner config` or `localrunner policy`:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_hand_edit_check_tells_edits_from_descriptions() {
    assert_eq!(hand_edits("Set `[env] path` in `runner.toml`.").len(), 1);
    assert_eq!(
        hand_edits("Edit `runner.toml`, then run `localrunner config set runner:a k v`.").len(),
        1
    );
    assert_eq!(hand_edits("Edit the file\n`runner.toml` by hand.").len(), 1);
    assert!(hand_edits("Never edit `runner.toml` by hand.").is_empty());
    assert_eq!(
        hand_edits("Put the clients in a `[[shared]]` block of `policy.toml`.").len(),
        1
    );
    assert_eq!(
        hand_edits("`[env] tools_dir = \"/opt/tools\"` in `runner.toml` appends `bin`.").len(),
        1
    );
    assert_eq!(
        hand_edits("Show the diff for `hub-policy.toml` and apply it.").len(),
        1
    );
    assert!(hand_edits("`init` writes `[env] path` of `runner.toml` from the shell.").is_empty());
    assert!(hand_edits(
        "Set it with `localrunner config set runner:<name> env.path <PATH>`: it edits `runner.toml`."
    )
    .is_empty());
    assert!(hand_edits("`localrunner config show logging` prints `logging.toml`.").is_empty());
    assert!(hand_edits(
        "Set it on the host: `localrunner --config-dir <dir> config set runner:<name> env.x 1` in `runner.toml`."
    )
    .is_empty());
    assert_eq!(
        hand_edits("Edit `policy.toml`, then run `localrunner policy check`.").len(),
        1
    );
}

#[test]
fn repo_setup_changes_machine_files_through_the_cli() {
    let text = skill("repo-setup");
    for needle in [
        "`--dry-run`",
        "`--restart`",
        "`localrunner config set runner:<name> <key> <value>`",
        "`localrunner policy allow-secret <name> --label <label>`",
        "`localrunner policy allow-service <name> --label <label>`",
        "Never set `allow_elevated` or `allow_same_user_as_broker`",
    ] {
        assert!(text.contains(needle), "repo-setup: {needle}");
    }
}

#[test]
fn run_skill_states_the_detach_rule() {
    let text = skill("run");
    assert!(
        text.contains("Without `--detach` the order is cancelled when the sending process exits")
    );
    assert!(text.contains("always passes `--detach` to `submit`"));
    assert!(!text.contains("only when the run must outlive the sending process"));
}
