mod common;

use common::TempDir;
use lr_cli::commands::service_cmd::{
    admin_left, autostart, autostart_with, broker_left_for_admin, elevated_command,
    install_service, install_service_with, report_lines, runner_service_command,
    runner_service_for, runner_service_with, update_service_on, update_service_with, version_line,
    ServiceContext, UpdateReport,
};
use lr_cli::commands::{RoleArg, Switch};
use lr_cli::doctor::same_user_risk_for;
use lr_cli::os_service::runner_service::{
    service_config_for, KeyAccess, KeyState, RunnerServiceInputs, ServiceConfig,
};
use lr_cli::os_service::{service_installed, Os};
use lr_cli::os_service::{Action, System};
use lr_cli::paths::Layout;
use std::cell::Cell;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const INSTALL: &str =
    "broker install-service needs administrator rights (run it elevated or with sudo)";
const OFF: &str = "autostart off broker needs administrator rights (run it elevated or with sudo)";

#[test]
fn broker_service_commands_without_administrator_rights_are_refused_before_any_change() {
    let dir = TempDir::new("no-admin");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let installed = service_installed(Os::current());
    assert_eq!(
        install_service_with(&layout, false, false),
        Err(INSTALL.to_string())
    );
    assert_eq!(
        autostart_with(
            &layout,
            Switch::Off,
            Some(RoleArg::Broker),
            None,
            false,
            false
        ),
        Err(OFF.to_string())
    );
    assert_eq!(
        autostart_with(
            &layout,
            Switch::On,
            Some(RoleArg::Broker),
            None,
            false,
            false
        ),
        Err(INSTALL.to_string())
    );
    assert_eq!(service_installed(Os::current()), installed);
    assert!(
        !dir.path().join("state").exists() && !dir.path().join("config").exists(),
        "nothing was written"
    );
}

#[test]
fn the_commands_read_the_rights_of_this_process() {
    if lr_cli::host::is_elevated() {
        eprintln!(
            "SKIPPED: the process is elevated; running the real commands here would install \
             or remove the broker service"
        );
        return;
    }
    let dir = TempDir::new("no-admin-real");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    assert_eq!(install_service(&layout, false), Err(INSTALL.to_string()));
    assert_eq!(
        autostart(
            &layout,
            Switch::Off,
            Some(RoleArg::Broker),
            None,
            false,
            false
        ),
        Err(OFF.to_string())
    );
}

fn joined_runner(layout: &Layout, name: &str) {
    let dir = layout.runner_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("broker.toml"),
        "address = \"127.0.0.1:1\"\nfingerprint = \"x\"\n",
    )
    .unwrap();
    lr_net::write_private(&dir.join("participant.key"), b"key").unwrap();
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn runner_service_without_administrator_rights_is_refused_with_the_exact_command() {
    let dir = TempDir::new("runner-service-no-admin");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    let exe = std::env::current_exe().unwrap();
    let absent = !lr_cli::os_service::runner_service::installed(Os::current(), "box");
    for (switch, word) in [(Switch::On, "on"), (Switch::Off, "off")] {
        if switch == Switch::Off && cfg!(windows) && absent {
            assert_eq!(
                runner_service_with(&layout, switch, "box", false, false),
                Ok(0),
                "off of a service that is not there is a no-op without rights"
            );
            continue;
        }
        let refused = runner_service_with(&layout, switch, "box", false, false).unwrap_err();
        let command = runner_service_command(Os::current(), &exe, switch, "box", &layout);
        assert!(
            refused.starts_with(&format!(
                "autostart {word} runner box --service needs administrator rights; "
            )),
            "{refused}"
        );
        assert!(refused.ends_with(&command), "{refused}");
        assert!(
            command.contains(&format!("autostart {word} runner box --service")),
            "{command}"
        );
        assert!(
            command.contains(&dir.path().join("state").display().to_string()),
            "the elevated run must find this user's state, not the administrator's: {command}"
        );
    }
    assert_eq!(entries(&layout.root), ["runner"], "nothing was written");
    assert_eq!(
        entries(&layout.runner_dir("box")),
        ["broker.toml", "participant.key"]
    );
    assert!(!dir.path().join("config").exists(), "nothing was written");
    let unix = runner_service_command(
        Os::Linux,
        Path::new("/home/alex/.local/bin/localrunner"),
        Switch::On,
        "box",
        &Layout::new("/home/alex/my state".into(), "/home/alex/my state".into()),
    );
    assert_eq!(
        unix,
        "sudo /home/alex/.local/bin/localrunner autostart on runner box --service --state-dir '/home/alex/my state'"
    );
    let windows = runner_service_command(
        Os::Windows,
        Path::new(r"C:\Users\Alex\bin\localrunner.exe"),
        Switch::Off,
        "box",
        &Layout::new(r"C:\Users\Alex\state".into(), r"C:\Users\Alex\cfg".into()),
    );
    assert_eq!(
        windows,
        r"& 'C:\Users\Alex\bin\localrunner.exe' autostart off runner box --service --state-dir 'C:\Users\Alex\state' --config-dir 'C:\Users\Alex\cfg'"
    );
}

#[test]
fn off_of_a_missing_windows_service_needs_no_administrator_rights() {
    let dir = TempDir::new("runner-service-off-absent");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    let plain = Fake {
        elevated: false,
        ..fake()
    };
    let off = service_for(&layout, &plain, &installing_user(), |ctx| {
        runner_service_for(&layout, Switch::Off, "box", false, ctx)
    });
    if cfg!(windows) {
        assert_eq!(off, Ok(0));
    } else {
        assert!(off.unwrap_err().contains("needs administrator rights"));
    }
    let installed = Fake {
        elevated: false,
        service: Some(Some(installing_user())),
        ..fake()
    };
    let e = service_for(&layout, &installed, &installing_user(), |ctx| {
        runner_service_for(&layout, Switch::Off, "box", false, ctx)
    })
    .unwrap_err();
    assert!(e.contains("needs administrator rights"), "{e}");
}

#[test]
fn runner_service_dry_run_needs_no_rights_and_a_service_needs_a_runner_name() {
    let dir = TempDir::new("runner-service-dry-run");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    assert_eq!(
        runner_service_with(&layout, Switch::On, "box", true, false),
        Ok(0)
    );
    assert_eq!(
        runner_service_with(&layout, Switch::Off, "box", true, false),
        Ok(0)
    );
    assert_eq!(
        entries(&layout.runner_dir("box")),
        ["broker.toml", "participant.key"],
        "a dry run writes nothing"
    );
    let typo = runner_service_with(&layout, Switch::On, "bx", true, false).unwrap_err();
    assert!(typo.contains("no runner bx"), "{typo}");
    assert_eq!(
        runner_service_with(&layout, Switch::Off, "bx", true, false),
        Ok(0),
        "a service of a runner whose directory is gone can still be removed"
    );
    let no_name = autostart(&layout, Switch::On, Some(RoleArg::Runner), None, true, true);
    assert!(
        no_name
            .as_ref()
            .is_err_and(|e| e.contains("autostart on runner <name> --service")),
        "{no_name:?}"
    );
    let hub = autostart(&layout, Switch::On, Some(RoleArg::Hub), None, true, true);
    assert!(hub.is_err(), "{hub:?}");
}

#[test]
fn a_ci_or_graphics_runner_is_refused_a_service_before_the_rights_are_checked() {
    let dir = TempDir::new("runner-service-refused");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let gpu = layout.runner_dir("gpu");
    std::fs::create_dir_all(&gpu).unwrap();
    std::fs::write(
        gpu.join("runner.toml"),
        "mem_mb = 1024\ncpu = 1\ntags = [\"android-emulator\"]\n",
    )
    .unwrap();
    let refused = runner_service_with(&layout, Switch::On, "gpu", true, false).unwrap_err();
    assert!(refused.contains("android-emulator"), "{refused}");
    let ci = runner_service_with(&layout, Switch::On, "ci", true, false).unwrap_err();
    assert!(ci.contains("`ci`"), "{ci}");
    assert_eq!(
        runner_service_with(&layout, Switch::Off, "gpu", true, false),
        Ok(0),
        "off is never refused"
    );
}

fn installing_user() -> String {
    if cfg!(windows) {
        format!("{}\\alex", lr_net::local_machine_name())
    } else {
        "alex".into()
    }
}

const ALEX: &str = "S-1-5-21-1-1001";

#[derive(Clone)]
struct Fake {
    user_sid: String,
    key: KeyState,
    dir: KeyState,
    login: bool,
    service: Option<Option<String>>,
    foreign: bool,
    elevated: bool,
}

fn owned_by(sid: &str) -> KeyState {
    KeyState::Read(KeyAccess {
        owner: Some(sid.into()),
        allowed: Some(vec![sid.into()]),
    })
}

fn fake() -> Fake {
    Fake {
        user_sid: if cfg!(windows) {
            ALEX.into()
        } else {
            "1000".into()
        },
        key: owned_by(if cfg!(windows) { ALEX } else { "1000" }),
        dir: owned_by(if cfg!(windows) { ALEX } else { "1000" }),
        login: false,
        service: None,
        foreign: false,
        elevated: true,
    }
}

fn service_for<R>(
    layout: &Layout,
    f: &Fake,
    user: &str,
    run: impl FnOnce(&ServiceContext) -> R,
) -> R {
    service_saying(layout, f, user, &|_| {}, run)
}

fn service_saying<R>(
    layout: &Layout,
    f: &Fake,
    user: &str,
    say: &dyn Fn(&str),
    run: impl FnOnce(&ServiceContext) -> R,
) -> R {
    let principal = |u: &str| -> Option<String> {
        let bare = u.rsplit('\\').next().unwrap_or(u).to_ascii_lowercase();
        Some(match bare.as_str() {
            "toor" => "0".into(),
            "localsvc" => "S-1-5-18".into(),
            _ => f.user_sid.clone(),
        })
    };
    let access = |p: &Path| -> KeyState {
        if p.ends_with("participant.key") {
            f.key.clone()
        } else {
            f.dir.clone()
        }
    };
    let login = |_: &str| f.login;
    let service = |name: &str| {
        f.service.clone().map(|owner| {
            Some({
                let named = owner.is_some();
                let owner = owner.unwrap_or_else(|| "nobody".into());
                let mut ours = service_config_for(&RunnerServiceInputs {
                    os: Os::Windows,
                    exe: std::env::current_exe().unwrap(),
                    name: name.to_string(),
                    state_dir: layout.root.clone(),
                    config_dir: (layout.config != layout.root).then(|| layout.config.clone()),
                    user: owner,
                    secret_files: Vec::new(),
                    granted_files: Vec::new(),
                });
                if !named {
                    ours.command = ours.command.replace(" --owner nobody", "");
                }
                match f.foreign {
                    true => ServiceConfig {
                        account: "LocalSystem".into(),
                        ..ours
                    },
                    false => ours,
                }
            })
        })
    };
    let started = |_: &str| Ok(());
    let stopped = |_: &str| true;
    let ctx = ServiceContext {
        user: user.to_string(),
        elevated: f.elevated,
        principal: &principal,
        access: &access,
        login_entry: &login,
        service: &service,
        confirm_started: &started,
        confirm_stopped: &stopped,
        say,
    };
    run(&ctx)
}

#[test]
fn the_service_notes_come_before_the_rights_are_asked_for_and_in_a_dry_run() {
    let dir = TempDir::new("runner-service-notes");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    std::fs::write(
        layout.runner_dir("box").join("runner.toml"),
        "tags = [\"docker\"]\n[secret.registry]\nfrom = \"env:LR_FAKE_REGISTRY_TOKEN\"\n",
    )
    .unwrap();
    let user = installing_user();
    let said = std::cell::RefCell::new(Vec::<String>::new());
    let say = |line: &str| said.borrow_mut().push(line.to_string());
    for (dry_run, elevated) in [(false, false), (true, false), (true, true)] {
        said.borrow_mut().clear();
        let f = Fake { elevated, ..fake() };
        let result = service_saying(&layout, &f, &user, &say, |ctx| {
            runner_service_for(&layout, Switch::On, "box", dry_run, ctx)
        });
        assert_eq!(result.is_ok(), dry_run, "{result:?}");
        let text = said.borrow().join("\n");
        assert!(text.contains("without a drain"), "{text}");
        assert!(text.contains("env:LR_FAKE_REGISTRY_TOKEN"), "{text}");
        assert!(text.contains("docker"), "{text}");
        assert_eq!(text.contains("DOMAIN"), cfg!(windows), "{text}");
    }
    said.borrow_mut().clear();
    let off = service_saying(&layout, &fake(), &user, &say, |ctx| {
        runner_service_for(&layout, Switch::Off, "box", true, ctx)
    });
    assert_eq!(off, Ok(0));
    assert!(said.borrow().is_empty(), "off has no notes");
}

#[test]
fn the_elevated_run_judges_the_same_user_rule_for_the_installing_user_not_for_root() {
    let dir = TempDir::new("runner-service-owner");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    std::fs::create_dir_all(&layout.config).unwrap();
    std::fs::write(
        layout.config_file("policy.toml"),
        "[[local_user]]\nname = \"alex\"\n",
    )
    .unwrap();
    joined_runner(&layout, "box");
    let user = installing_user();
    assert!(
        same_user_risk_for(&layout, true, Some("root")).is_some(),
        "under sudo the process user is root, and alex counts as another user"
    );
    assert_eq!(same_user_risk_for(&layout, true, Some(&user)), None);
    assert_eq!(
        service_for(&layout, &fake(), &user, |ctx| runner_service_for(
            &layout,
            Switch::On,
            "box",
            true,
            ctx
        )),
        Ok(0),
        "the init-written policy admits only the installing user: no refusal"
    );
}

#[test]
fn a_service_is_refused_for_a_missing_foreign_local_or_login_started_runner() {
    let dir = TempDir::new("runner-service-guards");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    let user = installing_user();
    let run = |f: &Fake, user: &str, switch: Switch, name: &str| {
        service_for(&layout, f, user, |ctx| {
            runner_service_for(&layout, switch, name, true, ctx)
        })
    };
    let on = |f: &Fake, name: &str| run(f, &user, Switch::On, name).unwrap_err();
    assert!(on(&fake(), "nope").contains("no runner nope"));
    let foreign = on(
        &Fake {
            dir: owned_by("S-1-5-21-1-1002"),
            ..fake()
        },
        "box",
    );
    assert!(foreign.contains("not owned by"), "{foreign}");
    let root_dir = on(
        &Fake {
            dir: owned_by("0"),
            ..fake()
        },
        "box",
    );
    assert!(root_dir.contains("not owned by"), "{root_dir}");
    let twice = on(
        &Fake {
            login: true,
            ..fake()
        },
        "box",
    );
    assert!(
        twice.contains("localrunner autostart off runner box"),
        "{twice}"
    );
    std::fs::create_dir_all(layout.runner_dir("local")).unwrap();
    let local = run(&fake(), &user, Switch::On, "local");
    if cfg!(windows) {
        let e = local.unwrap_err();
        assert!(e.contains("runner join"), "{e}");
    } else {
        assert_eq!(
            local,
            Ok(0),
            "on Unix the service runs as the user and reaches its broker"
        );
    }
    for who in ["root", r"NT AUTHORITY\SYSTEM", "toor", r"PC\localsvc"] {
        let e = run(&fake(), who, Switch::On, "box").unwrap_err();
        assert!(e.contains("never runs as"), "{who}: {e}");
    }
    let wrong = Fake {
        dir: owned_by("S-1-5-21-1-1002"),
        login: true,
        ..fake()
    };
    assert_eq!(
        run(&wrong, &user, Switch::Off, "box"),
        Ok(0),
        "off removes the service whatever else is wrong"
    );
}

#[test]
fn on_windows_the_key_is_handed_over_only_by_its_owner_and_given_back_to_the_service_owner() {
    if !cfg!(windows) {
        return;
    }
    let dir = TempDir::new("runner-service-key");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    joined_runner(&layout, "box");
    let run = |f: &Fake, user: &str, switch: Switch| {
        service_for(&layout, f, user, |ctx| {
            runner_service_for(&layout, switch, "box", true, ctx)
        })
    };
    let bob = Fake {
        user_sid: "S-1-5-21-1-1002".into(),
        dir: owned_by("S-1-5-21-1-1002"),
        ..fake()
    };
    let e = run(&bob, r"PC\bob", Switch::On).unwrap_err();
    assert!(
        e.contains(r"PC\bob") && e.contains(ALEX),
        "an administrator cannot hand alex's key over as himself: {e}"
    );
    let again = Fake {
        key: KeyState::Unreadable,
        service: Some(Some(r"PC\alex".into())),
        ..fake()
    };
    assert_eq!(
        run(&again, r"PC\alex", Switch::On),
        Ok(0),
        "a re-run over alex's own service"
    );
    let stranger = Fake {
        key: KeyState::Unreadable,
        ..fake()
    };
    assert!(run(&stranger, r"PC\alex", Switch::On)
        .unwrap_err()
        .contains("cannot read"));
    let no_owner = Fake {
        service: Some(None),
        ..fake()
    };
    let e = run(&no_owner, r"PC\bob", Switch::Off).unwrap_err();
    assert!(e.contains("takeown /F"), "{e}");
    let installed = Fake {
        service: Some(Some(r"PC\alex".into())),
        ..fake()
    };
    assert_eq!(run(&installed, r"PC\bob", Switch::Off), Ok(0));
    assert_eq!(
        run(&fake(), r"PC\alex", Switch::Off),
        Ok(0),
        "off without a service has nothing to stop"
    );
    let left = Fake {
        key: owned_by("S-1-5-80-1"),
        ..fake()
    };
    let e = run(&left, r"PC\alex", Switch::Off).unwrap_err();
    assert!(
        e.contains("takeown /F") && e.contains("no OS service"),
        "a key left with a removed service is named with the commands that give it back: {e}"
    );
    let foreign = Fake {
        service: Some(Some(r"PC\alex".into())),
        foreign: true,
        ..fake()
    };
    for switch in [Switch::On, Switch::Off] {
        let e = run(&foreign, r"PC\alex", switch).unwrap_err();
        assert!(
            e.contains("not one this installation registered"),
            "a service of the same name that is not ours is left alone: {e}"
        );
    }
}

#[test]
fn a_run_for_all_roles_leaves_only_the_broker_service_to_an_administrator() {
    assert!(broker_left_for_admin(None, false, false));
    assert!(broker_left_for_admin(Some(RoleArg::All), false, false));
    assert!(!broker_left_for_admin(Some(RoleArg::Broker), false, false));
    assert!(!broker_left_for_admin(None, false, true));
    assert!(!broker_left_for_admin(None, true, false));
    let on = admin_left(true);
    assert!(on.contains("autostart on broker"), "{on}");
    assert!(admin_left(false).contains("autostart off broker"));
}

#[test]
fn updating_the_service_needs_administrator_rights_and_an_installed_service() {
    let refused = update_service_with(false, false, true).unwrap_err();
    assert!(
        refused.starts_with("broker update-service needs administrator rights; run: "),
        "{refused}"
    );
    assert!(
        refused.ends_with(&elevated_command(
            Os::current(),
            &std::env::current_exe().unwrap()
        )),
        "{refused}"
    );
    assert!(update_service_with(false, true, false)
        .unwrap_err()
        .contains("install-service"));
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl System for Recorder {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.0.lock().unwrap().push(action.to_string());
        Ok(())
    }
}

#[test]
fn the_error_without_rights_contains_the_exact_command_for_each_os() {
    let exe = Path::new("/home/alex/.local/bin/localrunner");
    for os in [Os::Linux, Os::MacOs] {
        assert_eq!(
            elevated_command(os, exe),
            "sudo /home/alex/.local/bin/localrunner broker update-service"
        );
    }
    let windows = elevated_command(Os::Windows, Path::new(r"C:\Users\Alex\bin\localrunner.exe"));
    assert!(!windows.contains('\n'), "{windows}");
    assert!(
        windows.contains("Start-Process -Verb RunAs -Wait"),
        "{windows}"
    );
    assert!(
        windows.contains(r"C:\Users\Alex\bin\localrunner.exe"),
        "{windows}"
    );
    assert!(windows.contains("broker update-service"), "{windows}");
    assert!(windows.ends_with("Get-Content $o"), "{windows}");
    assert!(
        !windows.contains("-RedirectStandardOutput"),
        "RunAs cannot be combined with redirection: {windows}"
    );
    let quoted = elevated_command(Os::Windows, Path::new("C:\\it's\\localrunner.exe"));
    assert!(quoted.contains("it''s"), "{quoted}");
}

#[test]
fn dry_run_needs_no_rights_and_prints_the_version_change() {
    assert_eq!(update_service_with(true, false, true), Ok(0));
    assert_eq!(
        version_line(Some("0.1.0"), "0.1.1"),
        "service: 0.1.0 -> 0.1.1"
    );
    assert_eq!(version_line(None, "0.1.1"), "service: unknown -> 0.1.1");
}

fn read_after(old: &'static str, after: &'static str) -> impl Fn(&Path) -> Option<String> {
    let calls = Cell::new(0);
    move |_| {
        calls.set(calls.get() + 1);
        Some(if calls.get() == 1 { old } else { after }.to_string())
    }
}

#[test]
fn exit_code_is_zero_only_when_the_service_runs_the_new_version() {
    for (after, running_new) in [("0.1.1", true), ("0.1.0", false)] {
        let system = Recorder::default();
        let report = update_service_on(
            &system,
            Os::Linux,
            Path::new("/new/localrunner"),
            Path::new("/svc/localrunner"),
            &read_after("0.1.0", after),
            "0.1.1",
        )
        .unwrap();
        assert_eq!(report.running_new, running_new);
        assert_eq!(report.old.as_deref(), Some("0.1.0"));
        assert_eq!(system.0.lock().unwrap().len(), 3);
    }
}

#[test]
fn the_report_prints_old_new_and_downtime() {
    let up = UpdateReport {
        old: Some("0.1.0".into()),
        new: "0.1.1".into(),
        down: Duration::from_secs(3),
        running_new: true,
        unchanged: false,
    };
    assert_eq!(
        report_lines(&up),
        [
            "service: 0.1.0 -> 0.1.1",
            "service stopped and started, down for 3 s"
        ]
    );
    let stuck = UpdateReport {
        running_new: false,
        ..up
    };
    let lines = report_lines(&stuck).join("\n");
    assert!(lines.contains("still runs"), "{lines}");
}

fn update_with(system: &dyn System, old: &'static str, new: &str) -> Result<UpdateReport, String> {
    update_service_on(
        system,
        Os::Linux,
        Path::new("/new/localrunner"),
        Path::new("/svc/localrunner"),
        &|_: &Path| Some(old.to_string()),
        new,
    )
}

#[test]
fn a_service_already_on_this_version_is_left_alone() {
    let system = Recorder::default();
    let report = update_with(&system, "0.1.1", "0.1.1").unwrap();
    assert!(report.unchanged && report.running_new);
    assert!(system.0.lock().unwrap().is_empty());
    let lines = report_lines(&report).join("\n");
    assert!(lines.contains("already current"), "{lines}");
}

#[test]
fn a_service_newer_than_this_binary_is_not_replaced_by_it() {
    let system = Recorder::default();
    let err = update_with(&system, "0.2.0", "0.1.1").unwrap_err();
    assert!(err.contains("0.2.0") && err.contains("newer than"), "{err}");
    assert!(system.0.lock().unwrap().is_empty());
}

struct CopyFails(Mutex<Vec<String>>);

impl System for CopyFails {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.0.lock().unwrap().push(action.to_string());
        if matches!(action, Action::Copy { .. }) {
            Err("Text file busy".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_failed_copy_still_starts_the_stopped_service() {
    let system = CopyFails(Mutex::new(Vec::new()));
    let err = update_with(&system, "0.1.0", "0.1.1").unwrap_err();
    assert!(err.contains("Text file busy"), "{err}");
    let done = system.0.lock().unwrap().clone();
    assert_eq!(done.len(), 3, "{done:?}");
    assert!(done[2].contains("start"), "{done:?}");
    assert!(err.contains("the service was started again"), "{err}");
}

#[test]
fn autostart_on_without_a_role_leaves_the_broker_out_but_off_and_an_explicit_role_keep_it() {
    use lr_cli::commands::service_cmd::broker_in_scope;
    assert!(!broker_in_scope(None, true));
    assert!(broker_in_scope(None, false));
    assert!(broker_in_scope(Some(RoleArg::Broker), true));
    assert!(broker_in_scope(Some(RoleArg::All), true));
}

#[test]
fn autostart_status_lines_say_what_starts_how() {
    use lr_cli::commands::service_cmd::{autostart_status_lines, AutostartRow};
    let rows = vec![
        AutostartRow::new("hub", "hub", "login", None),
        AutostartRow::new("runner", "r1", "service", Some(true)),
        AutostartRow::new("runner", "r2", "none", None),
        AutostartRow::new("broker", "broker", "service", Some(false)),
    ];
    let lines = autostart_status_lines(&rows);
    assert_eq!(
        lines,
        vec![
            "hub: on (starts at user login)".to_string(),
            "runner r1: on (OS service, running)".to_string(),
            "runner r2: off".to_string(),
            "broker: on (OS service, stopped)".to_string(),
        ]
    );
    let json = lr_cli::commands::service_cmd::autostart_status_json(&rows);
    assert_eq!(json[1]["kind"], "service");
    assert_eq!(json[1]["running"], true);
    assert_eq!(json[2]["on"], false);
}
