use lr_cli::commands::confirm::Console;
use lr_cli::commands::rights::{ask_for_rights, retry_if_denied, Need, Outcome, Session};
use lr_cli::config_edit::Elevator;
use lr_cli::os_service::Os;
use std::cell::RefCell;
use std::io::Cursor;
use std::path::Path;

struct Fake {
    code: Result<i32, String>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Elevator for Fake {
    fn run_elevated(&self, _exe: &Path, args: &[String]) -> Result<i32, String> {
        self.calls.borrow_mut().push(args.to_vec());
        self.code.clone()
    }
}

fn fake(code: Result<i32, String>) -> Fake {
    Fake {
        code,
        calls: RefCell::new(Vec::new()),
    }
}

const NEED: Need<'static> = Need {
    what: "broker update-service",
    why: "replace the service binary",
};

fn plain() -> Session {
    Session {
        elevated: false,
        child: false,
        dry_run: false,
        no_elevate: false,
    }
}

fn ask(
    session: Session,
    terminal: bool,
    answer: &str,
    elevator: &Fake,
    os: Os,
) -> (Result<Outcome, String>, String) {
    let mut input = Cursor::new(answer.as_bytes().to_vec());
    let mut shown = Vec::new();
    let mut err = Vec::new();
    let args = vec!["broker".to_string(), "update-service".to_string()];
    let result = {
        let mut console = Console {
            terminal,
            input: &mut input,
            out: &mut shown,
        };
        ask_for_rights(
            &NEED,
            &session,
            &args,
            &mut console,
            elevator,
            os,
            Path::new("localrunner"),
            &mut err,
        )
    };
    (result, String::from_utf8(shown).unwrap())
}

#[test]
fn a_yes_runs_the_command_elevated_and_returns_its_code() {
    let e = fake(Ok(7));
    let (r, shown) = ask(plain(), true, "yes\n", &e, Os::Windows);
    assert_eq!(r, Ok(Outcome::Elevated(7)));
    assert!(shown.contains("needs administrator rights: replace the service binary"));
    assert!(shown.contains("UAC"));
    assert_eq!(e.calls.borrow().len(), 1);
}

#[test]
fn unix_names_sudo_in_the_question() {
    let e = fake(Ok(0));
    let (_, shown) = ask(plain(), true, "yes\n", &e, Os::Linux);
    assert!(shown.contains("sudo"));
}

#[test]
fn a_no_runs_nothing() {
    let e = fake(Ok(0));
    let (r, shown) = ask(plain(), true, "no\n", &e, Os::Linux);
    assert_eq!(r, Ok(Outcome::Declined));
    assert!(shown.contains("administrator rights were not granted"));
    assert!(e.calls.borrow().is_empty());
}

#[test]
fn without_a_terminal_it_names_the_command_and_asks_nothing() {
    let e = fake(Ok(0));
    let (r, shown) = ask(plain(), false, "yes\n", &e, Os::Linux);
    let msg = r.unwrap_err();
    assert!(msg.contains("needs administrator rights; run: sudo"));
    assert!(msg.contains("update-service"));
    assert!(shown.is_empty());
    assert!(e.calls.borrow().is_empty());
}

#[test]
fn no_elevate_never_asks() {
    let e = fake(Ok(0));
    let session = Session {
        no_elevate: true,
        ..plain()
    };
    let (r, shown) = ask(session, true, "yes\n", &e, Os::Linux);
    assert!(r.is_err());
    assert!(shown.is_empty());
    assert!(e.calls.borrow().is_empty());
}

#[test]
fn elevated_child_and_dry_run_run_here() {
    for session in [
        Session {
            elevated: true,
            ..plain()
        },
        Session {
            child: true,
            ..plain()
        },
        Session {
            dry_run: true,
            ..plain()
        },
    ] {
        let e = fake(Ok(0));
        let (r, shown) = ask(session, false, "", &e, Os::Windows);
        assert_eq!(r, Ok(Outcome::Here));
        assert!(shown.is_empty());
        assert!(e.calls.borrow().is_empty());
    }
}

#[test]
fn a_refused_prompt_is_reported_with_the_command_and_code_one() {
    let e = fake(Err("the UAC prompt was refused".into()));
    let (r, _) = ask(plain(), true, "yes\n", &e, Os::Windows);
    assert_eq!(r, Ok(Outcome::Elevated(1)));
}

const DENIED: &str =
    "the broker admin channel is not reachable: x: Permission denied (os error 13)";

#[test]
fn only_a_refused_admin_channel_is_retried_and_other_results_stay() {
    let session = Session {
        no_elevate: true,
        ..plain()
    };
    let args = vec!["runner".to_string(), "list".to_string()];
    assert_eq!(retry_if_denied(Ok(0), &NEED, &session, &args), Ok(0));
    let other = Err("the broker admin channel is not reachable: x: no such file".to_string());
    assert_eq!(
        retry_if_denied(other.clone(), &NEED, &session, &args),
        other
    );
    let unrelated = Err("name is taken".to_string());
    assert_eq!(
        retry_if_denied(unrelated.clone(), &NEED, &session, &args),
        unrelated
    );
}

#[test]
fn a_refused_admin_channel_without_a_way_to_ask_keeps_its_error() {
    let session = Session {
        no_elevate: true,
        ..plain()
    };
    let args = vec!["client".to_string(), "list".to_string()];
    let result = retry_if_denied(Err(DENIED.to_string()), &NEED, &session, &args);
    assert_eq!(result, Err(DENIED.to_string()));
}
