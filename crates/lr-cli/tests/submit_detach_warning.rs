use clap::CommandFactory;
use lr_cli::commands::jobs::{warn_if_orphaned, DETACH_WARNING};
use lr_cli::commands::Cli;

fn written(detach: bool, wait: bool, terminal: bool) -> String {
    let mut buf: Vec<u8> = Vec::new();
    warn_if_orphaned(detach, wait, terminal, &mut buf);
    String::from_utf8(buf).unwrap()
}

#[test]
fn warns_once_without_detach_wait_or_terminal() {
    assert_eq!(written(false, false, false), format!("{DETACH_WARNING}\n"));
}

#[test]
fn no_warning_with_detach() {
    assert!(written(true, false, false).is_empty());
}

#[test]
fn no_warning_with_wait() {
    assert!(written(false, true, false).is_empty());
}

#[test]
fn no_warning_with_a_terminal() {
    assert!(written(false, false, true).is_empty());
}

#[test]
fn warning_text_is_one_line_naming_the_cure() {
    assert!(!DETACH_WARNING.contains('\n'));
    assert!(DETACH_WARNING.contains("--detach"));
    assert!(DETACH_WARNING.contains("localrunner wait"));
    assert!(DETACH_WARNING.contains("localrunner run"));
}

#[test]
fn detach_flag_help_states_the_default() {
    let cmd = Cli::command();
    let submit = cmd.find_subcommand("submit").unwrap();
    let arg = submit
        .get_arguments()
        .find(|a| a.get_id() == "detach")
        .unwrap();
    let help = arg.get_help().unwrap().to_string();
    assert!(
        help.contains("cancelled when the sending process exits"),
        "{help}"
    );
}
