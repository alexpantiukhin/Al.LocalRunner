mod common;

use common::fake_roles::{with_runner, FakeRoles};
use common::TempDir;
use lr_cli::commands::admin_cmd::remove_asked;
use lr_cli::commands::confirm::{confirm, proceed, Console};
use lr_cli::commands::down_cmd::down_asked;
use lr_cli::commands::jobs::cancel_asked;
use lr_proto::{AdminRequest, DownMode};

const NO_YES: &str = "; confirm with --yes";

struct Typed {
    terminal: bool,
    input: &'static str,
}

const NO_TERMINAL: Typed = Typed {
    terminal: false,
    input: "yes\n",
};
const YES: Typed = Typed {
    terminal: true,
    input: "yes\n",
};
const NO: Typed = Typed {
    terminal: true,
    input: "no\n",
};

fn run<T>(typed: &Typed, f: impl FnOnce(&mut Console<'_>) -> T) -> (T, String) {
    let mut input = typed.input.as_bytes();
    let mut out = Vec::new();
    let result = f(&mut Console {
        terminal: typed.terminal,
        input: &mut input,
        out: &mut out,
    });
    (result, String::from_utf8(out).unwrap())
}

#[test]
fn yes_flag_confirms_without_a_prompt() {
    let (answer, out) = run(&NO_TERMINAL, |c| {
        confirm("x deletes y", "delete y?", true, c)
    });
    assert_eq!((answer, out), (Ok(true), String::new()));
}

#[test]
fn without_a_terminal_nothing_is_read_and_the_action_is_refused() {
    let (answer, out) = run(&NO_TERMINAL, |c| {
        confirm("x deletes y", "delete y?", false, c)
    });
    assert_eq!(answer, Err(format!("x deletes y{NO_YES}")));
    assert!(out.is_empty(), "{out}");
}

#[test]
fn on_a_terminal_only_yes_in_any_case_confirms() {
    let ask = |input| {
        run(
            &Typed {
                terminal: true,
                input,
            },
            |c| confirm("x deletes y", "delete y?", false, c),
        )
    };
    let (answer, out) = ask("yes\n");
    assert_eq!(answer, Ok(true));
    assert_eq!(out, "delete y? type yes: ");
    assert_eq!(ask("  YeS \n").0, Ok(true));
    for other in ["y\n", "no\n", "yess\n", "\n", ""] {
        assert_eq!(ask(other).0, Ok(false), "{other:?}");
    }
}

#[test]
fn proceed_says_aborted_when_not_confirmed() {
    let (answer, out) = run(&NO, |c| proceed("x deletes y", "delete y?", false, c));
    assert_eq!(answer, Ok(false));
    assert!(out.ends_with("aborted\n"), "{out}");
    let (answer, out) = run(&YES, |c| proceed("x deletes y", "delete y?", false, c));
    assert_eq!(answer, Ok(true));
    assert!(!out.contains("aborted"), "{out}");
}

#[tokio::test]
async fn down_cancel_asks_and_stops_nothing_unless_confirmed() {
    for (typed, yes, runs) in [
        (&NO_TERMINAL, false, false),
        (&NO, false, false),
        (&YES, false, true),
        (&NO_TERMINAL, true, true),
    ] {
        let dir = TempDir::new("confirm-down");
        let layout = with_runner(&dir);
        let roles = FakeRoles::new(false, 0);
        let mut input = typed.input.as_bytes();
        let mut out = Vec::new();
        let mut console = Console {
            terminal: typed.terminal,
            input: &mut input,
            out: &mut out,
        };
        let result = down_asked(
            &layout,
            Vec::new(),
            true,
            yes,
            &roles,
            std::future::pending::<()>(),
            &mut console,
        )
        .await;
        let told = roles.told.lock().unwrap().clone();
        if runs {
            assert_eq!(result, Ok(0));
            assert_eq!(told, vec![("r1".to_string(), DownMode::Cancel)]);
        } else if typed.terminal {
            assert_eq!(result, Ok(1));
            assert!(told.is_empty());
        } else {
            let err = result.unwrap_err();
            assert!(err.ends_with(NO_YES) && err.contains("--cancel"), "{err}");
            assert!(told.is_empty());
        }
        assert_eq!(roles.broker_stops() > 0, runs);
    }
}

#[tokio::test]
async fn plain_down_never_asks() {
    let dir = TempDir::new("confirm-drain");
    let layout = with_runner(&dir);
    let roles = FakeRoles::new(false, 0);
    let (result, out) = {
        let mut input = "".as_bytes();
        let mut out = Vec::new();
        let mut console = Console {
            terminal: false,
            input: &mut input,
            out: &mut out,
        };
        let r = down_asked(
            &layout,
            Vec::new(),
            false,
            false,
            &roles,
            std::future::pending::<()>(),
            &mut console,
        )
        .await;
        (r, String::from_utf8(out).unwrap())
    };
    assert_eq!(result, Ok(0));
    assert!(out.is_empty(), "{out}");
    assert_eq!(
        roles.told.lock().unwrap().clone(),
        vec![("r1".to_string(), DownMode::Drain)]
    );
}

fn removals() -> Vec<(&'static str, AdminRequest)> {
    vec![
        ("runner", AdminRequest::RunnerRemove { name: "r-1".into() }),
        ("client", AdminRequest::ClientRemove { name: "ci".into() }),
    ]
}

#[tokio::test]
async fn remove_asks_before_it_reaches_the_broker() {
    let dir = TempDir::new("confirm-remove");
    let layout = with_runner(&dir);
    for (kind, req) in removals() {
        let refused = {
            let mut input = "".as_bytes();
            let mut out = Vec::new();
            let mut c = Console {
                terminal: false,
                input: &mut input,
                out: &mut out,
            };
            remove_asked(&layout, req.clone(), false, &mut c).await
        };
        let err = refused.unwrap_err();
        assert!(
            err.contains("revokes") && err.ends_with(NO_YES),
            "{kind}: {err}"
        );

        let aborted = {
            let mut input = "no\n".as_bytes();
            let mut out = Vec::new();
            let mut c = Console {
                terminal: true,
                input: &mut input,
                out: &mut out,
            };
            remove_asked(&layout, req.clone(), false, &mut c).await
        };
        assert_eq!(aborted, Ok(1), "{kind}");

        for (terminal, input, yes) in [(true, "yes\n", false), (false, "", true)] {
            let mut input = input.as_bytes();
            let mut out = Vec::new();
            let mut c = Console {
                terminal,
                input: &mut input,
                out: &mut out,
            };
            let reached = remove_asked(&layout, req.clone(), yes, &mut c).await;
            let err = reached.unwrap_err();
            assert!(err.contains("not reachable"), "{kind}: {err}");
        }
    }
}

#[tokio::test]
async fn cancel_mine_asks_and_cancel_of_one_job_does_not() {
    let dir = TempDir::new("confirm-cancel");
    let layout = with_runner(&dir);
    let call = |typed: &'static Typed, job: Option<&str>, mine: bool, yes: bool| {
        let layout = layout.clone();
        let job = job.map(String::from);
        async move {
            let mut input = typed.input.as_bytes();
            let mut out = Vec::new();
            let mut c = Console {
                terminal: typed.terminal,
                input: &mut input,
                out: &mut out,
            };
            cancel_asked(&layout, job, "dev", mine, false, None, yes, &mut c).await
        }
    };
    let err = call(&NO_TERMINAL, None, true, false).await.unwrap_err();
    assert!(err.contains("--mine") && err.ends_with(NO_YES), "{err}");
    assert_eq!(call(&NO, None, true, false).await, Ok(1));
    for (typed, yes) in [(&YES, false), (&NO_TERMINAL, true)] {
        let err = call(typed, None, true, yes).await.unwrap_err();
        assert!(err.contains("hub is not running"), "{err}");
    }
    let err = call(&NO_TERMINAL, Some("job-1"), false, false)
        .await
        .unwrap_err();
    assert!(err.contains("hub is not running"), "{err}");
}

#[test]
fn the_binary_refuses_the_four_commands_without_a_terminal_and_without_yes() {
    let dir = TempDir::new("confirm-binary");
    let cases: [&[&str]; 4] = [
        &["down", "--cancel"],
        &["runner", "remove", "r-1"],
        &["client", "remove", "ci"],
        &["cancel", "--mine", "--as", "dev"],
    ];
    for args in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .args(args)
            .arg("--state-dir")
            .arg(dir.path())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary runs");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {err}");
        assert!(
            err.trim_end().ends_with("; confirm with --yes"),
            "{args:?}: {err}"
        );
    }
}
