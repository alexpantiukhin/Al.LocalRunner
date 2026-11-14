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
    if path.is_empty() && !rest.first().is_some_and(|w| w.starts_with('-')) {
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

#[test]
fn run_skill_states_the_detach_rule() {
    let text = skill("run");
    assert!(
        text.contains("Without `--detach` the order is cancelled when the sending process exits")
    );
    assert!(text.contains("always passes `--detach` to `submit`"));
    assert!(!text.contains("only when the run must outlive the sending process"));
}
