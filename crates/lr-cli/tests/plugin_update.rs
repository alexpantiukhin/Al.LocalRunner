use lr_cli::commands::plugin_update::{
    detect, offer_updates, outdated, parse_claude_list, parse_codex_list, run_updates, update_steps, Agent,
    Agents, Step,
};
use std::cell::RefCell;
use std::path::PathBuf;

const CLAUDE: &str = r#"[
  {"id":"localrunner@al-localrunner","version":"0.12.0","scope":"user","enabled":true},
  {"id":"localrunner@al-localrunner","version":"0.5.0","scope":"project","enabled":true,"projectPath":"D:\\work\\Al.Tabs"},
  {"id":"docker-skills@docker","version":"0.3.1","scope":"user","enabled":true}
]"#;

const CODEX: &str = "Marketplace `al-localrunner`
C:\\x\\marketplace.json

PLUGIN                      STATUS              VERSION  SOURCE
localrunner@al-localrunner  installed, enabled  0.5.0    https://github.com/a/b.git, path `plugin`, ref `main`
other@al-localrunner        available           1.0.0    x

Marketplace `docker`
PLUGIN                STATUS              VERSION  SOURCE
docker-skills@docker  installed, enabled  0.3.1    https://github.com/docker/skills.git
";

#[test]
fn the_localrunner_plugins_of_claude_code_are_read_with_scope_and_project() {
    let found = parse_claude_list(CLAUDE);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].agent, Agent::Claude);
    assert_eq!(found[0].version, "0.12.0");
    assert_eq!(found[0].scope.as_deref(), Some("user"));
    assert_eq!(found[1].version, "0.5.0");
    assert_eq!(found[1].scope.as_deref(), Some("project"));
    assert_eq!(found[1].project, Some(PathBuf::from("D:\\work\\Al.Tabs")));
    assert!(parse_claude_list("not json").is_empty());
}

#[test]
fn the_installed_localrunner_plugin_of_codex_is_read_and_an_available_one_is_not() {
    let found = parse_codex_list(CODEX);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].agent, Agent::Codex);
    assert_eq!(found[0].id, "localrunner@al-localrunner");
    assert_eq!(found[0].version, "0.5.0");
}

#[test]
fn only_a_plugin_older_than_the_new_version_is_outdated() {
    let mut all = parse_claude_list(CLAUDE);
    all.extend(parse_codex_list(CODEX));
    let old = outdated(&all, "0.12.0");
    assert_eq!(old.len(), 2, "{old:?}");
    assert!(old.iter().all(|p| p.version == "0.5.0"));
    assert!(outdated(&all, "0.5.0").is_empty());
    assert_eq!(outdated(&all, "0.13.0-rc1").len(), 3);
}

#[test]
fn the_steps_refresh_the_marketplace_then_update_the_plugin_in_its_own_scope() {
    let claude = &parse_claude_list(CLAUDE)[1];
    let steps = update_steps(claude);
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].program, "claude");
    assert_eq!(steps[0].args, ["plugin", "marketplace", "update", "al-localrunner"]);
    assert_eq!(steps[1].args, ["plugin", "update", "localrunner@al-localrunner", "--scope", "project", "--yes"]);
    assert_eq!(steps[1].cwd, Some(PathBuf::from("D:\\work\\Al.Tabs")));
    let codex = &parse_codex_list(CODEX)[0];
    let steps = update_steps(codex);
    assert_eq!(steps[0].args, ["plugin", "marketplace", "upgrade", "al-localrunner"]);
    assert_eq!(steps[1].args, ["plugin", "add", "localrunner@al-localrunner"]);
}

struct Fake {
    calls: RefCell<Vec<String>>,
    fail_on: Option<&'static str>,
}

impl Agents for Fake {
    fn run(&self, step: &Step) -> Result<String, String> {
        let line = format!("{} {}", step.program, step.args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        if self.fail_on.is_some_and(|f| line.contains(f)) {
            return Err("exit 1".into());
        }
        Ok(match line.as_str() {
            "claude plugin list --json" => CLAUDE.to_string(),
            "codex plugin list" => CODEX.to_string(),
            _ => String::new(),
        })
    }
}

#[test]
fn detection_asks_both_agents_and_an_agent_that_is_not_installed_is_skipped() {
    let both = Fake { calls: RefCell::new(vec![]), fail_on: None };
    assert_eq!(detect(&both).len(), 3);
    let none = Fake { calls: RefCell::new(vec![]), fail_on: Some("list") };
    assert!(detect(&none).is_empty());
}

#[test]
fn an_update_runs_its_steps_in_order_and_a_failed_step_is_reported_with_the_command_to_repeat() {
    let found = parse_claude_list(CLAUDE);
    let fake = Fake { calls: RefCell::new(vec![]), fail_on: None };
    let report = run_updates(&fake, &found[1..]);
    assert!(report[0].starts_with("ok"), "{report:?}");
    assert_eq!(fake.calls.borrow().len(), 2);
    let failing = Fake { calls: RefCell::new(vec![]), fail_on: Some("plugin update") };
    let report = run_updates(&failing, &found[1..]);
    assert!(report[0].starts_with("warn"), "{report:?}");
    assert!(report[0].contains("claude plugin update localrunner@al-localrunner"), "{report:?}");
}

fn console<'a>(
    terminal: bool,
    input: &'a mut std::io::Cursor<Vec<u8>>,
    out: &'a mut Vec<u8>,
) -> lr_cli::commands::confirm::Console<'a> {
    lr_cli::commands::confirm::Console { terminal, input, out }
}

#[test]
fn the_offer_lists_the_old_copies_and_updates_them_after_a_yes() {
    let fake = Fake { calls: RefCell::new(vec![]), fail_on: None };
    let (mut input, mut out) = (std::io::Cursor::new(b"yes\n".to_vec()), Vec::new());
    let lines = offer_updates(&fake, "0.12.0", false, false, &mut console(true, &mut input, &mut out));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| l.starts_with("ok")), "{lines:?}");
    let shown = String::from_utf8(out).unwrap();
    assert!(shown.contains("0.5.0 -> 0.12.0"), "{shown}");
    assert!(shown.contains("update the plugins now? type yes"), "{shown}");
}

#[test]
fn without_a_yes_nothing_is_changed_and_the_lines_name_the_commands() {
    let fake = Fake { calls: RefCell::new(vec![]), fail_on: None };
    let (mut input, mut out) = (std::io::Cursor::new(b"no\n".to_vec()), Vec::new());
    let lines = offer_updates(&fake, "0.12.0", false, false, &mut console(true, &mut input, &mut out));
    assert!(lines.iter().all(|l| l.starts_with("warn")), "{lines:?}");
    assert!(lines[0].contains("claude plugin marketplace update al-localrunner"), "{lines:?}");
    assert_eq!(fake.calls.borrow().len(), 2, "only the two list commands ran: {:?}", fake.calls);
    let (mut input, mut out) = (std::io::Cursor::new(Vec::new()), Vec::new());
    let lines = offer_updates(&fake, "0.12.0", false, false, &mut console(false, &mut input, &mut out));
    assert!(lines.iter().all(|l| l.starts_with("warn")), "no terminal: {lines:?}");
}

#[test]
fn dry_run_lists_and_current_plugins_say_nothing() {
    let fake = Fake { calls: RefCell::new(vec![]), fail_on: None };
    let (mut input, mut out) = (std::io::Cursor::new(Vec::new()), Vec::new());
    let lines = offer_updates(&fake, "0.12.0", false, true, &mut console(true, &mut input, &mut out));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(out.is_empty());
    assert!(offer_updates(&fake, "0.5.0", true, false, &mut console(true, &mut input, &mut out)).is_empty());
}
