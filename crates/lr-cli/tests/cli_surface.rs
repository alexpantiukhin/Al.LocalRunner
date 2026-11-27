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
        &["wait"],
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
        &[
            "localrunner",
            "runner",
            "run",
            "--name",
            "box",
            "--service",
            "--owner",
            r"PC\alex",
            "--state-dir",
            "/home/alex/state",
        ],
        &[
            "localrunner",
            "autostart",
            "on",
            "runner",
            "box",
            "--service",
            "--dry-run",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_ok(), "{args:?} refused");
    }
    let e = Cli::try_parse_from(["localrunner", "hub", "run", "--service"]).unwrap_err();
    assert_eq!(e.kind(), clap::error::ErrorKind::UnknownArgument);
    for args in [
        &["localrunner", "runner", "run", "--service"][..],
        &[
            "localrunner",
            "runner",
            "run",
            "--name",
            "box",
            "--owner",
            "alex",
        ],
    ] {
        let e = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(
            e.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "{args:?}: a runner service is one named runner, --owner only goes with it"
        );
    }
}

#[test]
fn wait_json_names_the_final_state_exit_code_and_reason() {
    use lr_cli::commands::jobs::{ended_json, Ended};
    use lr_core::{JobId, JobState};
    let job = JobId("j-1".into());
    let failed = Ended::Exit {
        state: JobState::Failed,
        exit_code: Some(3),
        reason: None,
    };
    assert_eq!(
        ended_json(&job, &failed),
        serde_json::json!({"job": "j-1", "state": "failed", "exit_code": 3, "reason": null})
    );
    assert_eq!(failed.process_code(), 3);
    assert_eq!(Ended::Rejected.process_code(), 1);
    assert_eq!(
        ended_json(&job, &Ended::Rejected)["state"],
        serde_json::Value::Null
    );
}

#[test]
fn cli_surface_lists_config() {
    let root = command();
    for (path, flags) in [
        (
            &["config", "set"][..],
            &["string", "dry-run", "restart"][..],
        ),
        (&["config", "unset"], &["dry-run", "restart"]),
        (&["config", "get"], &[]),
        (&["config", "show"], &[]),
    ] {
        let c = find(&root, path);
        for flag in flags {
            assert!(has_long(c, flag), "{path:?} has no --{flag}");
        }
    }
    for target in ["broker", "hub", "runner:r1", "logging"] {
        Cli::try_parse_from(["localrunner", "config", "set", target, "cpu", "-1"])
            .unwrap_or_else(|e| panic!("{target}: {e}"));
    }
    assert!(Cli::try_parse_from(["localrunner", "config", "show", "nobody"]).is_err());
    assert!(Cli::try_parse_from(["localrunner", "config", "show", "runner:"]).is_err());
}

#[test]
fn wait_takes_a_broker_like_log_history_and_status() {
    let root = command();
    for path in [&["wait"][..], &["log"], &["history"], &["status"]] {
        assert!(has_long(find(&root, path), "broker"), "{path:?}");
    }
    assert!(Cli::try_parse_from(["localrunner", "wait", "j1", "--broker", "vps"]).is_ok());
}

#[test]
fn secret_has_set_list_and_remove() {
    let root = command();
    for sub in ["set", "list", "remove"] {
        find(&root, &["secret", sub]);
    }
    assert!(Cli::try_parse_from(["localrunner", "secret", "set", "r1", "TOKEN", "--value-file", "f"]).is_ok());
    assert!(Cli::try_parse_from(["localrunner", "secret", "set", "r1", "TOKEN", "s3cret"]).is_err());
}
