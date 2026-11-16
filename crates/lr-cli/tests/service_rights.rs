mod common;

use common::TempDir;
use lr_cli::commands::service_cmd::{
    admin_left, autostart, autostart_with, broker_left_for_admin, elevated_command,
    install_service, install_service_with, report_lines, runner_service_command,
    runner_service_with, update_service_on, update_service_with, version_line, UpdateReport,
};
use lr_cli::commands::{RoleArg, Switch};
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

#[test]
fn runner_service_without_administrator_rights_is_refused_with_the_exact_command() {
    let dir = TempDir::new("runner-service-no-admin");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let exe = std::env::current_exe().unwrap();
    for (switch, word) in [(Switch::On, "on"), (Switch::Off, "off")] {
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
    assert!(
        !dir.path().join("state").exists() && !dir.path().join("config").exists(),
        "nothing was written"
    );
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
fn runner_service_dry_run_needs_no_rights_and_a_service_needs_a_runner_name() {
    let dir = TempDir::new("runner-service-dry-run");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    assert_eq!(
        runner_service_with(&layout, Switch::On, "box", true, false),
        Ok(0)
    );
    assert_eq!(
        runner_service_with(&layout, Switch::Off, "box", true, false),
        Ok(0)
    );
    assert!(
        !dir.path().join("state").exists(),
        "a dry run writes nothing"
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
