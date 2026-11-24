use clap::{CommandFactory, Parser};
use lr_cli::commands::jobs::{detach_hint, effective_detach};
use lr_cli::commands::{Cli, Command};
use lr_core::JobId;

fn submit_flags(args: &[&str]) -> (bool, bool, bool, bool) {
    let mut argv = vec!["localrunner", "submit", "unit", "--as", "claude:t"];
    argv.extend_from_slice(args);
    match Cli::try_parse_from(argv).unwrap().command {
        Command::Submit {
            order,
            wait,
            no_detach,
            quiet,
            ..
        } => (order.detach, wait, no_detach, quiet),
        other => panic!("{other:?}"),
    }
}

#[test]
fn submit_detaches_by_default() {
    assert!(effective_detach(false, false, false));
}

#[test]
fn no_detach_restores_the_old_behavior() {
    assert!(!effective_detach(false, true, false));
}

#[test]
fn submit_wait_and_run_stay_attached() {
    assert!(!effective_detach(false, false, true));
}

#[test]
fn explicit_detach_wins_everywhere() {
    assert!(effective_detach(true, false, true));
    assert!(effective_detach(true, false, false));
}

#[test]
fn flags_parse() {
    assert_eq!(submit_flags(&[]), (false, false, false, false));
    assert_eq!(submit_flags(&["--no-detach"]), (false, false, true, false));
    assert_eq!(submit_flags(&["--quiet"]), (false, false, false, true));
    assert_eq!(
        submit_flags(&["--detach", "--wait"]),
        (true, true, false, false)
    );
}

#[test]
fn no_detach_conflicts_with_detach() {
    let argv = [
        "localrunner",
        "submit",
        "--as",
        "a",
        "--detach",
        "--no-detach",
    ];
    assert!(Cli::try_parse_from(argv).is_err());
}

#[test]
fn run_has_no_no_detach_flag() {
    let cmd = Cli::command();
    let run = cmd.find_subcommand("run").unwrap();
    assert!(run.get_arguments().all(|a| a.get_id() != "no_detach"));
}

#[test]
fn hint_block_names_the_order_and_ready_commands() {
    let hint = detach_hint(&JobId("j-7".into()), Some("unit"), "claude:t");
    let lines: Vec<&str> = hint.lines().collect();
    assert_eq!(lines.len(), 4, "{hint}");
    assert_eq!(lines[0], "queued j-7 (profile unit, label claude:t)");
    assert_eq!(
        lines[1],
        "follow:  localrunner wait j-7   (run it as a background task; interrupting it cancels nothing)"
    );
    assert_eq!(
        lines[2],
        "result:  localrunner log j-7 --tail 50   (never read the whole log)"
    );
    assert_eq!(lines[3], "cancel:  localrunner cancel j-7 --as claude:t");
    assert!(hint.ends_with('\n'));
}

#[test]
fn hint_block_names_the_default_profile_when_none_is_given() {
    let hint = detach_hint(&JobId("j-1".into()), None, "a");
    assert!(
        hint.starts_with("queued j-1 (profile default, label a)\n"),
        "{hint}"
    );
}
