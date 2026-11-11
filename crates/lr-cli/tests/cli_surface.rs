use clap::{CommandFactory, Parser};
use lr_cli::commands::Cli;

fn command() -> clap::Command {
    let mut root = Cli::command().version("test");
    root.build();
    root
}

fn find<'a>(root: &'a clap::Command, path: &[&str]) -> &'a clap::Command {
    path.iter().fold(root, |c, name| {
        c.find_subcommand(name)
            .unwrap_or_else(|| panic!("no command {path:?}"))
    })
}

fn has_long(c: &clap::Command, long: &str) -> bool {
    c.get_arguments().any(|a| a.get_long() == Some(long))
}

#[test]
fn every_read_command_prints_json_on_request() {
    let root = command();
    let read: &[&[&str]] = &[
        &["jobs"],
        &["status"],
        &["explain"],
        &["history"],
        &["history", "show"],
        &["stats"],
        &["runners"],
        &["brokers"],
        &["doctor"],
        &["submit"],
    ];
    let missing: Vec<String> = read
        .iter()
        .filter(|p| !has_long(find(&root, p), "json"))
        .map(|p| p.join(" "))
        .collect();
    assert!(missing.is_empty(), "no --json: {missing:?}");
}

#[test]
fn roles_take_the_flags_the_os_starts_them_with() {
    for args in [
        &["localrunner", "hub", "run", "--background"][..],
        &[
            "localrunner",
            "runner",
            "run",
            "--name",
            "box",
            "--background",
        ],
        &["localrunner", "broker", "run", "--background"],
        &[
            "localrunner",
            "broker",
            "run",
            "--service",
            "--state-dir",
            "/var/lib/al-localrunner",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_ok(), "{args:?} refused");
    }
    for args in [
        &["localrunner", "hub", "run", "--service"][..],
        &["localrunner", "runner", "run", "--service"],
    ] {
        let e = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(
            e.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "{args:?}"
        );
    }
}
