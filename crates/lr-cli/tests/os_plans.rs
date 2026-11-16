use lr_cli::os_service::plans::{
    autostart_off, autostart_on, install_service, install_service_undo, task_name,
    uninstall_service, AutostartInputs, Entry, ServiceInputs,
};
use lr_cli::os_service::runner_service::{self, RunnerServiceInputs};
use lr_cli::os_service::{
    apply_or_undo, ensure_local_user, service_account, Action, Os, System, Tolerate,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

const ALL: [Os; 3] = [Os::Windows, Os::Linux, Os::MacOs];

fn inputs(os: Os) -> ServiceInputs {
    ServiceInputs {
        os,
        exe: PathBuf::from("/home/alex/.local/bin/localrunner"),
        binary: PathBuf::from("/usr/local/libexec/al-localrunner/localrunner"),
        root: PathBuf::from("/var/lib/al-localrunner"),
        configs: PathBuf::from("/home/alex/.local/share/al-localrunner"),
        user: "alex".into(),
    }
}

fn runs(plan: &[Action]) -> Vec<String> {
    plan.iter()
        .filter_map(|a| match a {
            Action::Run { program, args, .. } => Some(format!("{program} {}", args.join(" "))),
            _ => None,
        })
        .collect()
}

fn written(plan: &[Action]) -> Vec<(PathBuf, String)> {
    plan.iter()
        .filter_map(|a| match a {
            Action::WriteFile { path, contents } | Action::WriteUtf16 { path, contents } => {
                Some((path.clone(), contents.clone()))
            }
            _ => None,
        })
        .collect()
}

fn position(plan: &[Action], what: impl Fn(&Action) -> bool) -> Vec<usize> {
    plan.iter()
        .enumerate()
        .filter(|(_, a)| what(a))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn broker_service_runs_under_its_own_unprivileged_account_on_every_os() {
    for os in ALL {
        let plan = install_service(&inputs(os));
        let account = service_account(os);
        let text = format!("{:?}", plan);
        assert!(
            !text.contains("LocalSystem") && !text.contains("User=root"),
            "{os:?}"
        );
        assert!(
            plan.contains(&Action::EnsureLocalUser {
                policy: Path::new("/var/lib/al-localrunner").join("policy.toml"),
                user: "alex".into(),
            }),
            "{os:?}: the installing user is not admitted"
        );
        assert!(plan.contains(&Action::Copy {
            from: PathBuf::from("/home/alex/.local/bin/localrunner"),
            to: PathBuf::from("/usr/local/libexec/al-localrunner/localrunner"),
        }));
        let runs = runs(&plan);
        match os {
            Os::Windows => {
                assert!(account.starts_with(r"NT SERVICE\"));
                let create = runs
                    .iter()
                    .find(|r| r.starts_with("sc.exe create"))
                    .unwrap();
                assert!(create.contains(&format!("obj= {account}")), "{create}");
                assert!(
                    create.contains("broker run --service --state-dir"),
                    "{create}"
                );
                assert!(runs.contains(&"sc.exe failureflag AlLocalRunnerBroker 1".to_string()));
                assert!(runs.contains(
                    &"sc.exe failure AlLocalRunnerBroker reset= 86400 actions= restart/5000"
                        .to_string()
                ));
                assert!(
                    !runs.iter().any(|r| r.starts_with("icacls")),
                    "the DACL is set whole, not patched with icacls: {runs:?}"
                );
            }
            Os::Linux => {
                assert!(runs
                    .iter()
                    .any(|r| r.starts_with("useradd --system") && r.ends_with(account)));
                assert!(runs.iter().any(|r| r == "usermod -aG al-localrunner alex"));
                assert!(runs
                    .iter()
                    .any(|r| r
                        == &format!("chown -R {account}:al-localrunner /var/lib/al-localrunner")));
                assert!(runs.contains(&"chmod -R go-rwx /var/lib/al-localrunner".to_string()));
                assert!(runs.contains(&"chmod 0700 /var/lib/al-localrunner".to_string()));
                let (path, unit) = written(&plan).pop().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/etc/systemd/system/al-localrunner-broker.service")
                );
                assert!(unit.contains(&format!("User={account}")), "{unit}");
                assert!(unit.contains("UMask=0077"), "{unit}");
                assert!(
                    unit.contains("Restart=on-failure\nRestartSec=5\n"),
                    "{unit}"
                );
                assert!(unit.contains("RuntimeDirectory=al-localrunner"), "{unit}");
                assert!(unit.contains("\"broker\" \"run\" \"--service\""), "{unit}");
                assert!(runs
                    .iter()
                    .any(|r| r == "systemctl enable --now al-localrunner-broker.service"));
            }
            Os::MacOs => {
                assert!(plan.iter().any(|a| matches!(
                    a,
                    Action::EnsureMacAccount { name, .. } if name == account
                )));
                assert!(runs.contains(&"chmod 0710 /var/lib/al-localrunner".to_string()));
                let ipc = Path::new("/var/lib/al-localrunner").join("ipc");
                assert!(runs.contains(&format!("chmod 0750 {}", ipc.display())));
                let (path, daemon) = written(&plan).pop().unwrap();
                assert!(path.starts_with("/Library/LaunchDaemons"));
                assert!(daemon.contains(&format!("<string>{account}</string>")));
                assert!(daemon.contains("<key>Umask</key>\n  <integer>63</integer>"));
                assert!(
                    daemon.contains(
                        "<key>KeepAlive</key>\n  <true/>\n  <key>ThrottleInterval</key>\n  <integer>5</integer>"
                    ),
                    "{daemon}"
                );
                assert!(runs
                    .iter()
                    .any(|r| r.starts_with("launchctl bootstrap system")));
            }
        }
    }
}

#[test]
fn the_state_directory_is_checked_and_closed_before_any_configuration_goes_in() {
    for os in ALL {
        let plan = install_service(&inputs(os));
        let secured = position(&plan, |a| {
            matches!(a, Action::SecureServiceDir { path, account }
                if path == Path::new("/var/lib/al-localrunner") && account == service_account(os))
        });
        let writes = position(&plan, |a| {
            matches!(
                a,
                Action::CopyIfMissing { .. } | Action::EnsureLocalUser { .. }
            )
        });
        assert!(!secured.is_empty(), "{os:?}");
        assert_eq!(writes.len(), 3, "{os:?}");
        assert!(secured[0] < writes[0], "{os:?}: {plan:#?}");
        assert!(
            !plan.iter().any(
                |a| matches!(a, Action::CreateDir(p) if p == Path::new("/var/lib/al-localrunner"))
            ),
            "{os:?}: the directory is only created by the check"
        );
        if os == Os::Windows {
            let runs = runs(&plan);
            let create = runs
                .iter()
                .find(|r| r.starts_with("sc.exe create"))
                .unwrap();
            assert!(create.contains("start= demand"), "{create}");
            let auto = position(&plan, |a| {
                matches!(a, Action::Run { program, args, .. }
                    if program == "sc.exe" && args.join(" ") == "config AlLocalRunnerBroker start= auto")
            });
            let started = position(
                &plan,
                |a| matches!(a, Action::Run { program, args, .. } if program == "sc.exe" && args[0] == "start"),
            );
            assert_eq!(auto.len(), 1);
            assert!(
                *secured.last().unwrap() < auto[0] && auto[0] < started[0],
                "the service starts at boot only once everything passed: {plan:#?}"
            );
            let created = position(
                &plan,
                |a| matches!(a, Action::Run { program, args, .. } if program == "sc.exe" && args[0] == "create"),
            );
            assert!(
                created[0] < secured[0],
                "the service account exists before the directory is given to it"
            );
            assert!(
                *secured.last().unwrap() > *writes.last().unwrap(),
                "copied files end up owned by the service"
            );
        }
    }
}

#[test]
fn a_failed_install_removes_only_a_service_it_registered() {
    let undo = install_service_undo(Os::Windows, false);
    assert_eq!(
        runs(&undo),
        vec![
            "sc.exe stop AlLocalRunnerBroker".to_string(),
            "sc.exe delete AlLocalRunnerBroker".to_string()
        ]
    );
    for a in &undo {
        assert!(
            matches!(
                a,
                Action::Run {
                    tolerate: Tolerate::Anything,
                    ..
                }
            ),
            "{a}"
        );
    }
    assert!(install_service_undo(Os::Windows, true).is_empty());
    assert!(install_service_undo(Os::Linux, false).is_empty());
    assert!(install_service_undo(Os::MacOs, false).is_empty());
}

struct Failing {
    done: RefCell<Vec<String>>,
}

impl System for Failing {
    fn apply(&self, action: &Action) -> Result<(), String> {
        self.done.borrow_mut().push(action.to_string());
        match action {
            Action::SecureServiceDir { .. } => Err("someone else owns it".into()),
            _ => Ok(()),
        }
    }
}

#[test]
fn a_rejected_directory_leaves_no_service_registered() {
    let plan = install_service(&inputs(Os::Windows));
    let undo = install_service_undo(Os::Windows, false);
    let system = Failing {
        done: RefCell::new(Vec::new()),
    };
    let err = apply_or_undo(&system, &plan, &undo).unwrap_err();
    assert!(err.contains("someone else owns it"), "{err}");
    let done = system.done.into_inner();
    let tail: Vec<String> = undo.iter().map(|a| a.to_string()).collect();
    assert!(done.ends_with(&tail), "{done:#?}");
    assert!(!done.iter().any(|d| d.contains("start= auto")), "{done:#?}");

    let ok = Failing {
        done: RefCell::new(Vec::new()),
    };
    let quiet: Vec<Action> = plan
        .iter()
        .filter(|a| !matches!(a, Action::SecureServiceDir { .. }))
        .cloned()
        .collect();
    apply_or_undo(&ok, &quiet, &undo).unwrap();
    assert_eq!(
        ok.done.into_inner().len(),
        quiet.len(),
        "no undo on success"
    );
}

#[test]
fn removing_the_service_keeps_state_and_tolerates_a_missing_service() {
    let binary = Path::new("/usr/local/libexec/al-localrunner/localrunner");
    for os in ALL {
        let plan = uninstall_service(os, binary);
        assert!(
            !plan.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
            "{os:?}"
        );
        assert_eq!(plan.last(), Some(&Action::RemoveFile(binary.to_path_buf())));
        for a in &plan {
            if let Action::Run {
                tolerate, program, ..
            } = a
            {
                assert_eq!(
                    tolerate == &Tolerate::Nothing,
                    program == "icacls",
                    "{os:?}: a right that is not taken back is reported, the rest is tolerated: {a}"
                );
            }
        }
    }
}

fn autostart(os: Os, entry: Entry) -> AutostartInputs {
    AutostartInputs {
        os,
        exe: PathBuf::from("/opt/bin/localrunner"),
        entry,
        state_dir: PathBuf::from("/home/alex/state & more"),
        config_dir: None,
        home: PathBuf::from("/home/alex"),
        uid: 1000,
        user: r"PC\alex".into(),
        principal: "S-1-5-21-1-2-3-1001".into(),
    }
}

#[test]
fn windows_autostart_is_a_logon_task_of_the_user_restarted_on_failure_without_a_window() {
    let runner = autostart(Os::Windows, Entry::Runner(Some("box".into())));
    let plan = autostart_on(&runner);
    let (file, task) = written(&plan).pop().unwrap();
    assert_eq!(
        file,
        Path::new("/home/alex/state & more").join("autostart-runner-box.xml")
    );
    assert!(matches!(plan[0], Action::WriteUtf16 { .. }));
    let launcher = format!(
        "<Command>{}</Command>",
        Path::new("/opt/bin/localrunner")
            .with_file_name("localrunnerw.exe")
            .display()
    );
    assert!(task.contains(&launcher), "{task}");
    for part in [
        "<LogonTrigger>",
        r"<UserId>PC\alex</UserId>",
        "<LogonType>InteractiveToken</LogonType>",
        "<RunLevel>LeastPrivilege</RunLevel>",
        "<RestartOnFailure>\n      <Interval>PT1M</Interval>\n      <Count>999</Count>\n    </RestartOnFailure>",
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
        "<Arguments>runner run --name box --background --state-dir &quot;/home/alex/state &amp; more&quot;</Arguments>",
    ] {
        assert!(task.contains(part), "{part} missing in {task}");
    }
    let runs = runs(&plan);
    assert_eq!(
        runs,
        vec![format!(
            "schtasks /Create /TN Al.LocalRunner runner-box S-1-5-21-1-2-3-1001 /XML {} /F",
            file.display()
        )]
    );
    assert_eq!(plan.last(), Some(&Action::RemoveFile(file)));
    let off = autostart_off(&runner);
    assert_eq!(
        off,
        vec![Action::Run {
            program: "schtasks".into(),
            args: [
                "/Delete",
                "/TN",
                "Al.LocalRunner runner-box S-1-5-21-1-2-3-1001",
                "/F"
            ]
            .map(String::from)
            .to_vec(),
            tolerate: Tolerate::Anything,
        }]
    );
    let hub = autostart_off(&autostart(Os::Windows, Entry::Hub));
    assert!(runs_of(&hub)[0].contains("Al.LocalRunner hub S-1-5-21-1-2-3-1001"));
    let other = AutostartInputs {
        user: r"PC\bob".into(),
        principal: "S-1-5-21-1-2-3-1002".into(),
        ..autostart(Os::Windows, Entry::Runner(Some("box".into())))
    };
    assert_ne!(task_name(&runner), task_name(&other));
    assert!(!runs_of(&autostart_off(&other))[0].contains("1001"));
}

fn runs_of(plan: &[Action]) -> Vec<String> {
    runs(plan)
}

#[test]
fn unix_autostart_starts_the_role_as_the_user_at_logon_and_off_removes_it() {
    let runner = autostart(Os::Linux, Entry::Runner(Some("box".into())));
    let plan = autostart_on(&runner);
    let (path, unit) = written(&plan).pop().unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/alex/.config/systemd/user/al-localrunner-runner-box.service")
    );
    assert!(
        unit.contains("\"runner\" \"run\" \"--name\" \"box\" \"--background\""),
        "{unit}"
    );
    assert!(
        !unit.contains("UMask"),
        "a user's own processes keep the user's umask: {unit}"
    );
    assert!(
        runs(&plan).contains(&"systemctl --user enable al-localrunner-runner-box.service".into())
    );
    assert!(autostart_off(&runner).contains(&Action::RemoveFile(path)));

    let mac = autostart(Os::MacOs, Entry::Runner(None));
    let plan = autostart_on(&mac);
    let (path, agent) = written(&plan).pop().unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/alex/Library/LaunchAgents/com.al-localrunner.runner.plist")
    );
    assert!(agent.contains("<string>runner</string>") && !agent.contains("UserName"));
    assert!(!agent.contains("Umask"), "{agent}");
    assert!(agent.contains("<string>/home/alex/state &amp; more</string>"));
    assert!(runs(&plan)
        .iter()
        .any(|r| r.starts_with("launchctl bootstrap gui/1000")));
}

#[test]
fn installing_user_is_added_to_local_users_once() {
    let empty = ensure_local_user("", "alex").unwrap().unwrap();
    let file = lr_broker::policy::load(&empty).unwrap();
    assert_eq!(file.local_users.len(), 1);
    assert_eq!(file.local_users[0].name, "alex");
    assert_eq!(ensure_local_user(&empty, "alex").unwrap(), None);
    let two = ensure_local_user(&empty, "ci-agent").unwrap().unwrap();
    assert_eq!(lr_broker::policy::load(&two).unwrap().local_users.len(), 2);
}

#[test]
fn autostart_on_linux_hints_at_enable_linger_and_nowhere_else() {
    use lr_cli::commands::service_cmd::linger_hint;
    assert_eq!(
        linger_hint(Os::Linux, true, false),
        Some("to start without logging in: loginctl enable-linger")
    );
    assert_eq!(linger_hint(Os::Linux, false, false), None);
    assert_eq!(linger_hint(Os::Linux, true, true), None);
    assert_eq!(linger_hint(Os::Windows, true, false), None);
    assert_eq!(linger_hint(Os::MacOs, true, false), None);
}

fn runner_service(os: Os, name: &str) -> RunnerServiceInputs {
    RunnerServiceInputs {
        os,
        exe: PathBuf::from("/home/alex/.local/bin/localrunner"),
        name: name.into(),
        state_dir: PathBuf::from("/home/alex/state & more"),
        config_dir: None,
        user: "alex".into(),
    }
}

fn root_like(text: &str) -> bool {
    text.contains("User=root")
        || text.contains("<string>root</string>")
        || text.contains("LocalSystem")
        || text.contains(r"NT AUTHORITY\SYSTEM")
        || text.contains("obj= SYSTEM")
}

#[test]
fn broker_service_and_login_autostart_never_create_a_runner_service_and_the_runner_service_is_never_root(
) {
    for os in ALL {
        let service = format!("{:?}", install_service(&inputs(os)));
        assert!(
            !service.contains("runner run") && !service.contains("\"runner\""),
            "{os:?}: broker install-service installs only the broker"
        );
        let runner = format!("{:?}", runner_service::install(&runner_service(os, "box")));
        assert!(!root_like(&runner), "{os:?}: {runner}");
        for entry in [Entry::Runner(None), Entry::Runner(Some("gpu".into()))] {
            let plan = autostart_on(&autostart(os, entry));
            for r in runs(&plan) {
                let system_wide = r.starts_with("sc.exe")
                    || r.contains("New-Service")
                    || (r.starts_with("systemctl") && !r.starts_with("systemctl --user"))
                    || r.contains("system/");
                assert!(
                    !system_wide,
                    "{os:?}: login autostart starts a runner as a service: {r}"
                );
            }
            for (path, _) in written(&plan) {
                let p = path.to_string_lossy().replace('\\', "/");
                assert!(
                    p.starts_with("/home/alex/"),
                    "{os:?}: the runner's entry is the user's own: {p}"
                );
            }
        }
    }
}

#[test]
fn updating_the_service_stops_it_copies_the_binary_and_starts_it_without_touching_the_registration()
{
    use lr_cli::os_service::plans::update_service;
    for os in [Os::Windows, Os::Linux, Os::MacOs] {
        let plan = update_service(
            os,
            Path::new("/new/localrunner"),
            Path::new("/svc/localrunner"),
        );
        let text: Vec<String> = plan.iter().map(|a| a.to_string()).collect();
        assert_eq!(plan.len(), 3, "{text:?}");
        assert!(
            matches!(&plan[1], Action::Copy { from, to }
                if from == Path::new("/new/localrunner") && to == Path::new("/svc/localrunner")),
            "{text:?}"
        );
        let all = text.join(
            "
",
        );
        assert!(!all.contains("create") && !all.contains("delete"), "{all}");
    }
}

#[test]
fn runner_service_runs_under_a_non_root_account_on_every_os() {
    for os in ALL {
        let i = runner_service(os, "box");
        assert_eq!(runner_service::check_identity(&i), Ok(()), "{os:?}");
        let plan = runner_service::install(&i);
        let runs = runs(&plan);
        match os {
            Os::Windows => {
                assert_eq!(runner_service::service_name(os, "box"), "AlLocalRunner-box");
                assert_eq!(runner_service::account(&i), r"NT SERVICE\AlLocalRunner-box");
                let create = runs
                    .iter()
                    .find(|r| r.starts_with("sc.exe create AlLocalRunner-box "))
                    .unwrap();
                assert!(
                    create.contains(r"obj= NT SERVICE\AlLocalRunner-box"),
                    "{create}"
                );
                assert!(create.contains("start= demand"), "{create}");
                assert!(
                    runs.contains(
                        &"sc.exe privs AlLocalRunner-box SeChangeNotifyPrivilege/SeCreateGlobalPrivilege"
                            .to_string()
                    ),
                    "the service token keeps no impersonation privilege: {runs:?}"
                );
                assert!(runs.contains(
                    &"sc.exe failure AlLocalRunner-box reset= 86400 actions= restart/5000"
                        .to_string()
                ));
                let auto = config_auto(&runs);
                let start = runs
                    .iter()
                    .position(|r| r == "sc.exe start AlLocalRunner-box")
                    .unwrap();
                let grant = runs.iter().position(|r| r.starts_with("icacls")).unwrap();
                assert!(grant < auto && auto < start, "{runs:?}");
            }
            Os::Linux => {
                let (path, unit) = written(&plan).pop().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/etc/systemd/system/al-localrunner-runner-box.service")
                );
                assert!(unit.contains("\nUser=alex\n"), "{unit}");
                assert!(unit.contains("\nNoNewPrivileges=true\n"), "{unit}");
                assert!(unit.contains("\nRestart=on-failure\n"), "{unit}");
                assert!(unit.contains("WantedBy=multi-user.target"), "{unit}");
                assert_eq!(
                    runs,
                    vec![
                        "systemctl daemon-reload".to_string(),
                        "systemctl enable --now al-localrunner-runner-box.service".to_string()
                    ]
                );
            }
            Os::MacOs => {
                let (path, daemon) = written(&plan).pop().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/Library/LaunchDaemons/com.al-localrunner.runner-box.plist")
                );
                assert!(
                    daemon.contains("<key>UserName</key>\n  <string>alex</string>"),
                    "{daemon}"
                );
                assert!(
                    daemon.contains(
                        "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>"
                    ),
                    "a runner stopped by `down` stays stopped: {daemon}"
                );
                assert!(runs.contains(&format!("launchctl bootstrap system {}", path.display())));
            }
        }
        for root in ["root", "SYSTEM", r"NT AUTHORITY\SYSTEM", "LocalSystem"] {
            let refused = runner_service::check_identity(&RunnerServiceInputs {
                user: root.into(),
                ..runner_service(os, "box")
            })
            .unwrap_err();
            assert!(refused.contains(root), "{os:?}: {refused}");
        }
    }
}

#[test]
fn runner_service_plan_names_the_runner_and_state_dir() {
    for os in ALL {
        let mut i = runner_service(os, "box");
        i.config_dir = Some(PathBuf::from("/home/alex/config"));
        let plan = runner_service::install(&i);
        let args = runner_service::run_args(&i);
        assert_eq!(args[0], "/home/alex/.local/bin/localrunner");
        let words = args[1..].join(" ");
        assert!(
            words.starts_with("runner run --name box --service"),
            "{os:?}: {words}"
        );
        assert!(
            words.contains("--state-dir /home/alex/state & more --config-dir /home/alex/config"),
            "{os:?}: {words}"
        );
        assert_eq!(
            words.contains("--owner alex"),
            os == Os::Windows,
            "{os:?}: only the Windows service account differs from the installing user: {words}"
        );
        match os {
            Os::Windows => {
                let runs = runs(&plan);
                let create = runs
                    .iter()
                    .find(|r| r.starts_with("sc.exe create"))
                    .unwrap();
                assert!(
                    create.contains(
                        r#"runner run --name box --service --owner alex --state-dir "/home/alex/state & more""#
                    ),
                    "{create}"
                );
                for (dir, rights) in [
                    ("/home/alex/state & more", "M"),
                    ("/home/alex/config", "RX"),
                    ("/home/alex/.local/bin", "RX"),
                ] {
                    let grant = format!(
                        r"icacls {} /grant NT SERVICE\AlLocalRunner-box:(OI)(CI){rights}",
                        Path::new(dir).display()
                    );
                    assert!(runs.contains(&grant), "{grant} missing in {runs:?}");
                }
            }
            Os::Linux => {
                let (_, unit) = written(&plan).pop().unwrap();
                assert!(
                    unit.contains(
                        "ExecStart=\"/home/alex/.local/bin/localrunner\" \"runner\" \"run\" \"--name\" \"box\" \"--service\" \"--state-dir\" \"/home/alex/state & more\""
                    ),
                    "{unit}"
                );
            }
            Os::MacOs => {
                let (_, daemon) = written(&plan).pop().unwrap();
                assert!(daemon.contains("<string>box</string>"), "{daemon}");
                assert!(
                    daemon.contains("<string>/home/alex/state &amp; more</string>"),
                    "{daemon}"
                );
            }
        }
    }
}

#[test]
fn runner_service_off_removes_only_that_runner() {
    for os in ALL {
        let plan = runner_service::uninstall(&runner_service(os, "box"), false);
        let text: Vec<String> = plan.iter().map(|a| a.to_string()).collect();
        let all = text.join("\n");
        assert!(!all.contains("Broker") && !all.contains("broker"), "{all}");
        assert!(
            !plan.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
            "{os:?}: the runner's state stays"
        );
        for a in &plan {
            if let Action::Run {
                tolerate, program, ..
            } = a
            {
                assert_eq!(
                    tolerate == &Tolerate::Nothing,
                    program == "icacls",
                    "{os:?}: a right that is not taken back is reported, the rest is tolerated: {a}"
                );
            }
        }
        let runs = runs(&plan);
        match os {
            Os::Windows => {
                let stop = runs
                    .iter()
                    .position(|r| r == "sc.exe stop AlLocalRunner-box")
                    .unwrap();
                let revoke = runs
                    .iter()
                    .position(|r| r.starts_with("icacls") && r.contains("/remove"))
                    .unwrap();
                let delete = runs
                    .iter()
                    .position(|r| r == "sc.exe delete AlLocalRunner-box")
                    .unwrap();
                assert!(
                    stop < revoke && revoke < delete,
                    "the account is named while it still exists: {runs:?}"
                );
            }
            Os::Linux => {
                assert!(runs
                    .contains(&"systemctl disable --now al-localrunner-runner-box.service".into()));
                assert!(plan.contains(&Action::RemoveFile(PathBuf::from(
                    "/etc/systemd/system/al-localrunner-runner-box.service"
                ))));
            }
            Os::MacOs => {
                assert!(
                    runs.contains(&"launchctl bootout system/com.al-localrunner.runner-box".into())
                );
                assert!(plan.contains(&Action::RemoveFile(PathBuf::from(
                    "/Library/LaunchDaemons/com.al-localrunner.runner-box.plist"
                ))));
            }
        }
        let other = format!(
            "{:?}",
            runner_service::uninstall(&runner_service(os, "gpu"), true)
        );
        assert!(!other.contains("box"), "{os:?}: {other}");
    }
}

fn key_of(i: &RunnerServiceInputs) -> String {
    i.state_dir
        .join("runner")
        .join(&i.name)
        .join("participant.key")
        .display()
        .to_string()
}

fn config_auto(runs: &[String]) -> usize {
    let configs: Vec<usize> = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("sc.exe config AlLocalRunner-box "))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        configs.len(),
        1,
        "one `sc config` at the end changes an existing service, after the risky steps: {runs:#?}"
    );
    let line = &runs[configs[0]];
    assert!(
        line.contains("start= auto")
            && line.contains("binPath=")
            && line.contains(r"obj= NT SERVICE\AlLocalRunner-box"),
        "{line}"
    );
    configs[0]
}

fn at(runs: &[String], line: &str) -> usize {
    runs.iter()
        .position(|r| r == line)
        .unwrap_or_else(|| panic!("{line} missing in {runs:#?}"))
}

#[test]
fn windows_runner_service_hands_the_runner_key_to_the_service_account_before_it_starts() {
    let i = runner_service(Os::Windows, "box");
    let plan = runner_service::install(&i);
    let runs = runs(&plan);
    let key = key_of(&i);
    let create = runs
        .iter()
        .position(|r| r.starts_with("sc.exe create"))
        .unwrap();
    let take = at(&runs, &format!("takeown /F {key} /A"));
    let grant = at(
        &runs,
        &format!(r"icacls {key} /inheritance:r /grant:r NT SERVICE\AlLocalRunner-box:(F)"),
    );
    let user_out = at(&runs, &format!("icacls {key} /remove alex"));
    let auto = config_auto(&runs);
    let start = at(&runs, "sc.exe start AlLocalRunner-box");
    let privs = at(
        &runs,
        "sc.exe privs AlLocalRunner-box SeChangeNotifyPrivilege/SeCreateGlobalPrivilege",
    );
    let failure = at(&runs, "sc.exe failureflag AlLocalRunner-box 1");
    let dirs = runs
        .iter()
        .position(|r| r.starts_with("icacls") && r.contains("(OI)(CI)"))
        .unwrap();
    assert!(
        create < dirs
            && dirs < take
            && take < grant
            && grant < user_out
            && user_out < privs
            && privs < failure
            && failure < auto
            && auto < start,
        "the administrators take the key first, so a re-run over a service that already holds \
         it works; the key is the account's alone before the start: {runs:#?}"
    );
    assert!(
        !runs.iter().any(|r| r.contains("/setowner")),
        "the administrators stay the owner, which the key check accepts; no step needs the \
         restore privilege: {runs:#?}"
    );
    assert_key_steps_are_checked(&plan, &key);
    for os in [Os::Linux, Os::MacOs] {
        let text = format!("{:?}", runner_service::install(&runner_service(os, "box")));
        assert!(
            !text.contains("participant.key"),
            "{os:?}: the user's key stays the user's"
        );
    }
}

#[test]
fn windows_runner_service_off_gives_the_key_back_while_the_account_still_exists() {
    let i = runner_service(Os::Windows, "box");
    let plan = runner_service::uninstall(&i, true);
    let runs = runs(&plan);
    let key = key_of(&i);
    let stop = at(&runs, "sc.exe stop AlLocalRunner-box");
    let take = at(&runs, &format!("takeown /F {key} /A"));
    let grant = at(
        &runs,
        &format!("icacls {key} /inheritance:r /grant:r alex:(F)"),
    );
    let service_out = at(
        &runs,
        &format!(r"icacls {key} /remove NT SERVICE\AlLocalRunner-box"),
    );
    let delete = at(&runs, "sc.exe delete AlLocalRunner-box");
    assert!(
        stop < take && take < grant && grant < service_out && service_out < delete,
        "{runs:#?}"
    );
    assert!(!runs.iter().any(|r| r.contains("/setowner")), "{runs:#?}");
    assert_key_steps_are_checked(&plan, &key);
    let kept = format!("{:?}", runner_service::uninstall(&i, false));
    assert!(
        !kept.contains("participant.key"),
        "no key to give back: {kept}"
    );
}

fn assert_key_steps_are_checked(plan: &[Action], key: &str) {
    let steps: Vec<&Action> = plan
        .iter()
        .filter(|a| matches!(a, Action::Run { args, .. } if args.iter().any(|w| w == key)))
        .collect();
    assert_eq!(steps.len(), 3, "{steps:#?}");
    for a in steps {
        assert!(
            matches!(
                a,
                Action::Run {
                    tolerate: Tolerate::Nothing,
                    ..
                }
            ),
            "a failed key step must stop the plan, not pass as success: {a}"
        );
    }
}

const ALEX: &str = "S-1-5-21-1-1001";
const SERVICE: &str = "S-1-5-80-1";
const ADMINS: &str = "S-1-5-32-544";

fn read(owner: &str, allowed: &[&str]) -> runner_service::KeyState {
    runner_service::KeyState::Read(runner_service::KeyAccess {
        owner: Some(owner.into()),
        allowed: Some(allowed.iter().map(|s| s.to_string()).collect()),
    })
}

#[test]
fn the_key_goes_to_the_service_only_from_its_owner_or_back_to_the_service_owner() {
    use runner_service::{key_handover_refusal, restore_owner, KeyState};
    let refusal = |user: &str, id: &str, key: &KeyState, service: Option<Option<String>>| {
        key_handover_refusal(
            "box",
            user,
            Some(id),
            Some(SERVICE),
            Path::new(r"C:\s\runner\box\participant.key"),
            key,
            service,
        )
    };
    assert_eq!(refusal("PC\\alex", ALEX, &read(ALEX, &[ALEX]), None), None);
    assert_eq!(
        refusal("PC\\alex", ALEX, &read(ADMINS, &[ALEX]), None),
        None,
        "a key written from an elevated shell, or given back by off, is still alex's"
    );
    let bob = refusal("PC\\bob", "S-1-5-21-1-1002", &read(ALEX, &[ALEX]), None).unwrap();
    assert!(
        bob.contains("PC\\bob") && bob.contains(ALEX) && bob.contains("runner box"),
        "{bob}"
    );
    assert_eq!(
        refusal(
            "PC\\alex",
            ALEX,
            &read(ADMINS, &[SERVICE]),
            Some(Some("PC\\alex".into()))
        ),
        None,
        "a re-run over alex's own service, which holds the key"
    );
    assert_eq!(
        refusal(
            "PC\\alex",
            ALEX,
            &KeyState::Unreadable,
            Some(Some("PC\\alex".into()))
        ),
        None,
        "a re-run over a key an older version handed over with the account as its owner"
    );
    for service in [None, Some(None), Some(Some("PC\\bob".into()))] {
        let left = refusal("PC\\alex", ALEX, &read(ADMINS, &[SERVICE]), service.clone()).unwrap();
        assert!(
            left.contains("takeown /F") && left.contains("PC\\alex:(F)"),
            "a key left with the service account names the commands that give it back: {service:?}: {left}"
        );
        let e = refusal("PC\\alex", ALEX, &KeyState::Unreadable, service.clone()).unwrap();
        assert!(e.contains("cannot read"), "{service:?}: {e}");
    }
    let missing = refusal("PC\\alex", ALEX, &KeyState::Missing, None).unwrap();
    assert!(missing.contains("runner join"), "{missing}");

    let key = Path::new(r"C:\Users\alex\state\runner\box\participant.key");
    assert_eq!(
        restore_owner("box", key, false, Some(Some("PC\\alex".into()))),
        Ok(None)
    );
    assert_eq!(
        restore_owner("box", key, true, None),
        Ok(None),
        "no service, nothing to give back"
    );
    assert_eq!(
        restore_owner("box", key, true, Some(Some("PC\\alex".into()))),
        Ok(Some("PC\\alex".into())),
        "the key goes back to the user the service was installed for, not to whoever runs off"
    );
    let unknown = restore_owner("box", key, true, Some(None)).unwrap_err();
    assert!(
        unknown.contains("takeown /F") && unknown.contains("<user>"),
        "{unknown}"
    );
}

#[test]
fn the_service_owner_is_read_from_the_service_command_line() {
    use runner_service::owner_from_command_line;
    assert_eq!(
        owner_from_command_line(
            r"C:\bin\localrunner.exe runner run --name box --service --owner PC\alex --state-dir C:\s"
        ),
        Some("PC\\alex".into())
    );
    assert_eq!(
        owner_from_command_line(
            r#""C:\Program Files\lr.exe" runner run --name box --service --owner "PC\alex smith" --state-dir "C:\my state""#
        ),
        Some("PC\\alex smith".into())
    );
    assert_eq!(
        owner_from_command_line(
            r#""C:\Users\Алексей\lr.exe" runner run --name box --service --owner "ПК\Алексей Петров" --state-dir "C:\Users\Алексей\state""#
        ),
        Some("ПК\\Алексей Петров".into()),
        "the service configuration is read as UTF-16, so a Cyrillic account survives"
    );
    assert_eq!(
        owner_from_command_line(r"C:\bin\localrunner.exe runner run --name box --service"),
        None
    );
}

#[test]
fn the_key_given_back_is_checked_and_a_failure_prints_the_manual_command() {
    use runner_service::{key_recovery, restore_verdict, KeyState};
    let key = Path::new(r"C:\s\runner\box\participant.key");
    let command = key_recovery(key, "PC\\alex", r"NT SERVICE\AlLocalRunner-box");
    assert_eq!(
        command,
        concat!(
            "\n  takeown /F \"C:\\s\\runner\\box\\participant.key\" /A",
            "\n  icacls \"C:\\s\\runner\\box\\participant.key\" /inheritance:r /grant:r \"PC\\alex:(F)\"",
            "\n  icacls \"C:\\s\\runner\\box\\participant.key\" /remove \"NT SERVICE\\AlLocalRunner-box\"",
        ),
        "one command per line: they work in cmd and in PowerShell alike"
    );
    let verdict = |state: &KeyState| {
        restore_verdict(
            "box",
            key,
            state,
            "PC\\alex",
            Some(ALEX),
            r"NT SERVICE\AlLocalRunner-box",
        )
    };
    assert_eq!(verdict(&read(ALEX, &[ALEX])), Ok(None));
    assert_eq!(verdict(&read(ADMINS, &[ALEX])), Ok(None));
    for wrong in [
        read(SERVICE, &[SERVICE]),
        read(ADMINS, &[SERVICE]),
        read(ALEX, &[ALEX, SERVICE]),
        read(ALEX, &[]),
        read("S-1-5-21-1-1002", &[ALEX]),
    ] {
        let e = verdict(&wrong).unwrap_err();
        assert!(e.contains("runner box") && e.ends_with(&command), "{e}");
    }
    let note = verdict(&KeyState::Unreadable).unwrap().unwrap();
    assert!(
        note.contains("icacls") && note.contains("PC\\alex"),
        "{note}"
    );
}

#[test]
fn the_key_handed_to_the_service_is_checked_and_a_failure_prints_the_manual_command() {
    use runner_service::{handover_verdict, KeyState};
    let key = Path::new(r"C:\s\runner\box\participant.key");
    let verdict = |state: &KeyState| {
        handover_verdict(
            "box",
            key,
            state,
            r"NT SERVICE\AlLocalRunner-box",
            Some(SERVICE),
            "PC\\alex",
        )
    };
    assert_eq!(verdict(&read(ADMINS, &[SERVICE])), Ok(()));
    assert_eq!(verdict(&read(SERVICE, &[SERVICE])), Ok(()));
    for wrong in [
        read(ADMINS, &[SERVICE, ALEX]),
        read(ALEX, &[SERVICE]),
        read(ADMINS, &[ALEX]),
        KeyState::Unreadable,
    ] {
        let e = verdict(&wrong).unwrap_err();
        assert!(
            e.contains("runner box") && e.contains("takeown /F") && e.contains("PC\\alex:(F)"),
            "{e}"
        );
    }
}

#[derive(Default)]
struct FailOn {
    fail_on: Vec<&'static str>,
    fail_second: Vec<&'static str>,
    exists: bool,
    applied: RefCell<Vec<String>>,
}

impl System for FailOn {
    fn apply(&self, action: &Action) -> Result<(), String> {
        let line = match action {
            Action::Run { program, args, .. } => format!("{program} {}", args.join(" ")),
            other => other.to_string(),
        };
        let earlier = self
            .applied
            .borrow()
            .iter()
            .filter(|l| self.fail_second.iter().any(|f| l.contains(f)))
            .count();
        self.applied.borrow_mut().push(line.clone());
        let tolerate = match action {
            Action::Run { tolerate, .. } => tolerate.clone(),
            _ => Tolerate::Nothing,
        };
        if self.exists && line.starts_with("sc.exe create") {
            return match tolerate {
                Tolerate::Nothing => Err("exit code 1073: the service exists".into()),
                _ => Ok(()),
            };
        }
        let fails = self.fail_on.iter().any(|f| line.contains(f))
            || (earlier == 1 && self.fail_second.iter().any(|f| line.contains(f)));
        match fails && tolerate != Tolerate::Anything {
            true => Err("exit code 5: denied".into()),
            false => Ok(()),
        }
    }
}

fn fail_on(steps: &[&'static str]) -> FailOn {
    FailOn {
        fail_on: steps.to_vec(),
        ..FailOn::default()
    }
}

#[test]
fn a_failed_first_install_is_undone_and_gives_the_key_back() {
    let i = runner_service(Os::Windows, "box");
    let key = key_of(&i);
    let system = fail_on(&["sc.exe start"]);
    let e = runner_service::apply_install(&system, &i, false).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert!(
        applied.contains(&format!("icacls {key} /inheritance:r /grant:r alex:(F)"))
            && applied.contains("sc.exe delete AlLocalRunner-box"),
        "{applied}"
    );
    assert!(e.contains("undone"), "{e}");
}

#[test]
fn a_first_install_that_failed_before_the_key_steps_leaves_the_key_alone() {
    let i = runner_service(Os::Windows, "box");
    let system = fail_on(&["(OI)(CI)M"]);
    let e = runner_service::apply_install(&system, &i, false).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert!(!applied.contains("takeown"), "{applied}");
    assert!(
        applied.contains("sc.exe delete AlLocalRunner-box"),
        "{applied}"
    );
    assert!(e.contains("undone"), "{e}");
}

#[test]
fn a_re_run_that_failed_before_its_config_leaves_the_service_definition_as_it_was() {
    let i = runner_service(Os::Windows, "box");
    for step in ["(OI)(CI)M", "takeown", "sc.exe privs", "sc.exe failure "] {
        let system = fail_on(&[step]);
        let e = runner_service::apply_install(&system, &i, true).unwrap_err();
        let applied = system.applied.borrow().join("\n");
        assert!(
            !applied.contains("sc.exe delete")
                && !applied.contains("sc.exe stop")
                && !applied.contains("sc.exe config"),
            "{step}: {applied}"
        );
        assert!(
            e.contains("existed before") && e.contains("definition was not changed"),
            "{step}: {e}"
        );
        assert!(!e.contains("undone"), "{step}: {e}");
    }
}

#[test]
fn a_re_run_that_failed_at_the_start_says_the_definition_was_updated() {
    let i = runner_service(Os::Windows, "box");
    let system = fail_on(&["sc.exe start"]);
    let e = runner_service::apply_install(&system, &i, true).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert!(
        !applied.contains("sc.exe delete") && !applied.contains("sc.exe stop"),
        "{applied}"
    );
    assert!(
        e.contains("existed before")
            && e.contains("definition was updated")
            && !e.contains("left as it was")
            && !e.contains("undone"),
        "{e}"
    );
}

#[test]
fn a_service_created_by_someone_else_after_the_check_is_never_undone() {
    let i = runner_service(Os::Windows, "box");
    let system = FailOn {
        exists: true,
        fail_on: vec!["sc.exe start"],
        ..FailOn::default()
    };
    let e = runner_service::apply_install(&system, &i, false).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert!(
        !applied.contains("sc.exe delete") && !applied.contains("sc.exe stop"),
        "sc create answered 1073: the service is not this run's: {applied}"
    );
    assert!(e.contains("existed before"), "{e}");
    let ok = FailOn {
        exists: true,
        ..FailOn::default()
    };
    assert_eq!(runner_service::apply_install(&ok, &i, false), Ok(()));
}

#[test]
fn an_undo_stops_at_a_failed_key_step_and_never_deletes_the_service_after_it() {
    let i = runner_service(Os::Windows, "box");
    let key = key_of(&i);
    let system = FailOn {
        fail_on: vec!["sc.exe start"],
        fail_second: vec!["takeown"],
        ..FailOn::default()
    };
    let e = runner_service::apply_install(&system, &i, false).unwrap_err();
    let applied = system.applied.borrow();
    let handed = applied
        .iter()
        .position(|l| l == &format!("icacls {key} /remove alex"))
        .expect("the install handed the key over");
    let undo_take = applied
        .iter()
        .rposition(|l| l.starts_with("takeown"))
        .unwrap();
    assert!(handed < undo_take, "{applied:#?}");
    assert!(
        !applied.iter().any(|l| l.contains("sc.exe delete")),
        "the service stays until the key is back: {applied:#?}"
    );
    assert!(
        e.contains("takeown /F") && e.contains("localrunner autostart off runner box --service"),
        "{e}"
    );
    assert!(!e.contains("undone"), "{e}");
}

#[test]
fn off_waits_for_the_service_to_stop_before_it_touches_the_key() {
    let i = runner_service(Os::Windows, "box");
    let system = FailOn::default();
    let e = runner_service::apply_uninstall(&system, &i, true, &mut || false).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert_eq!(applied, "sc.exe stop AlLocalRunner-box", "{applied}");
    assert!(
        e.contains("did not stop") && e.contains("Stop-Service AlLocalRunner-box"),
        "{e}"
    );
    let system = FailOn::default();
    assert_eq!(
        runner_service::apply_uninstall(&system, &i, true, &mut || true),
        Ok(())
    );
    assert!(system
        .applied
        .borrow()
        .iter()
        .any(|l| l == "sc.exe delete AlLocalRunner-box"));
}

#[test]
fn off_reports_rights_it_could_not_take_back_and_still_removes_the_service() {
    let i = runner_service(Os::Windows, "box");
    let system = fail_on(&[r"/remove NT SERVICE\AlLocalRunner-box"]);
    let e = runner_service::apply_uninstall(&system, &i, false, &mut || true).unwrap_err();
    let applied = system.applied.borrow().join("\n");
    assert!(
        applied.contains("sc.exe delete AlLocalRunner-box"),
        "{applied}"
    );
    let state = Path::new("/home/alex/state & more").display().to_string();
    assert!(
        e.contains("not taken back")
            && e.contains(&format!(
                r#"icacls "{state}" /remove "NT SERVICE\AlLocalRunner-box""#
            )),
        "{e}"
    );
    let key_fails = fail_on(&["takeown"]);
    let e = runner_service::apply_uninstall(&key_fails, &i, true, &mut || true).unwrap_err();
    assert!(
        !key_fails
            .applied
            .borrow()
            .iter()
            .any(|l| l.contains("sc.exe delete")),
        "a key not given back keeps the service: {e}"
    );
}

#[test]
fn the_service_is_confirmed_running_or_stopped_by_polling_a_probe() {
    use runner_service::confirm_state;
    let run = |states: &[Option<bool>], want: bool| {
        let mut left = states.iter().copied();
        let mut pauses = 0;
        let r = confirm_state(
            &mut || left.next().unwrap_or(Some(false)),
            &mut || pauses += 1,
            6,
            3,
            want,
        );
        (r, pauses)
    };
    assert_eq!(
        run(&[Some(false), Some(true), Some(true), Some(true)], true).0,
        Ok(())
    );
    assert_eq!(
        run(
            &[
                Some(true),
                Some(true),
                Some(false),
                Some(false),
                Some(false),
                Some(false)
            ],
            true
        ),
        (Err(Some(false)), 5),
        "a runner that starts and then stops (bad config, missing tool, key error) fails"
    );
    assert_eq!(run(&[None; 6], true).0, Err(None));
    assert_eq!(run(&[Some(false); 3], false).0, Ok(()));
    assert_eq!(run(&[Some(true); 6], false), (Err(Some(true)), 5));
}

#[test]
fn a_start_failure_names_the_log_and_the_state_command() {
    let line = runner_service::start_failure(
        Os::Windows,
        "box",
        Some(false),
        Path::new(r"C:\s\logs\service.log"),
    );
    assert!(
        line.contains("did not keep running")
            && line.contains(r"C:\s\logs\service.log")
            && line.contains("Get-Service AlLocalRunner-box"),
        "{line}"
    );
}

#[test]
fn the_service_command_line_is_split_with_windows_quoting() {
    use runner_service::{owner_from_command_line, split_command_line};
    assert_eq!(
        owner_from_command_line(
            r#""C:\Tools --owner ACME\alice\localrunner.exe" runner run --name box --service --owner PC\alex --state-dir C:\s"#
        ),
        Some(r"PC\alex".into())
    );
    assert_eq!(
        split_command_line(r#""C:\a b\lr.exe" x "y z" "q\"r" C:\d\ "e\\""#),
        vec![r"C:\a b\lr.exe", "x", "y z", "q\"r", r"C:\d\", r"e\",]
    );
}

#[test]
fn only_a_service_this_installation_registered_is_changed_or_removed() {
    use runner_service::{not_ours, service_config_for, ServiceConfig};
    let i = runner_service(Os::Windows, "box");
    let ours = service_config_for(&i);
    assert_eq!(not_ours("box", &ours, &i), None);
    let mut other_dir = i.clone();
    other_dir.state_dir = PathBuf::from(r"C:\other");
    let mut other_name = i.clone();
    other_name.name = "gpu".into();
    let mut other_exe = i.clone();
    other_exe.exe = PathBuf::from(r"C:\bin\something.exe");
    for (what, cfg) in [
        ("another state dir", service_config_for(&other_dir)),
        ("another runner", service_config_for(&other_name)),
        ("another program", service_config_for(&other_exe)),
        (
            "another account",
            ServiceConfig {
                account: "LocalSystem".into(),
                ..ours.clone()
            },
        ),
        (
            "not a runner service",
            ServiceConfig {
                command: r"C:\bin\localrunner.exe broker run --service".into(),
                ..ours.clone()
            },
        ),
    ] {
        let e = not_ours("box", &cfg, &i).unwrap_or_else(|| panic!("{what} passed"));
        assert!(
            e.contains("AlLocalRunner-box") && e.contains("not one this installation registered"),
            "{what}: {e}"
        );
    }
}

#[test]
fn a_failed_linux_install_names_the_command_that_cleans_up() {
    let i = runner_service(Os::Linux, "box");
    let system = fail_on(&["systemctl enable"]);
    let e = runner_service::apply_install(&system, &i, false).unwrap_err();
    assert!(
        e.contains("localrunner autostart off runner box --service"),
        "{e}"
    );
}

#[test]
fn a_failed_windows_install_names_the_command_that_cleans_up() {
    let e = runner_service::install_failure(Os::Windows, "box", "icacls ...: exit 5".into());
    assert!(
        e.starts_with("icacls ...: exit 5")
            && e.contains("localrunner autostart off runner box --service"),
        "{e}"
    );
}

#[test]
fn root_is_refused_by_its_id_as_well_as_by_its_name() {
    for (user, id) in [("toor", "0"), ("svc", "S-1-5-18")] {
        let e = runner_service::identity_refusal(user, Some(id)).unwrap();
        assert!(e.contains(user), "{e}");
    }
    assert_eq!(runner_service::identity_refusal("alex", Some("1000")), None);
    assert!(runner_service::identity_refusal("root", None).is_some());
}

#[test]
fn a_directory_belongs_to_the_user_by_its_owner_id() {
    use runner_service::{dir_owned, KeyAccess, KeyState};
    let owned = |owner: &str| {
        KeyState::Read(KeyAccess {
            owner: Some(owner.into()),
            allowed: None,
        })
    };
    assert!(dir_owned(Os::Linux, &owned("1000"), Some("1000")));
    assert!(!dir_owned(Os::Linux, &owned("0"), Some("1000")));
    assert!(dir_owned(
        Os::Windows,
        &owned("S-1-5-21-1"),
        Some("S-1-5-21-1")
    ));
    assert!(dir_owned(
        Os::Windows,
        &owned("S-1-5-32-544"),
        Some("S-1-5-21-1")
    ));
    assert!(!dir_owned(
        Os::Windows,
        &owned("S-1-5-21-2"),
        Some("S-1-5-21-1")
    ));
    assert!(!dir_owned(
        Os::Windows,
        &KeyState::Unreadable,
        Some("S-1-5-21-1")
    ));
    assert!(!dir_owned(Os::Linux, &owned("1000"), None));
}

#[test]
fn a_login_autostart_is_refused_for_a_runner_that_is_an_os_service() {
    let e = runner_service::service_conflict(Os::Linux, "box", true).unwrap();
    assert!(
        e.contains("al-localrunner-runner-box")
            && e.contains("localrunner autostart off runner box --service"),
        "{e}"
    );
    assert_eq!(
        runner_service::service_conflict(Os::Linux, "box", false),
        None
    );
}

#[test]
fn a_local_ipc_runner_is_refused_a_windows_service_and_a_joined_one_is_not() {
    let refused = runner_service::local_runner_refusal(Os::Windows, "box", false).unwrap();
    assert!(
        refused.contains("runner join") && refused.contains("box"),
        "{refused}"
    );
    assert_eq!(
        runner_service::local_runner_refusal(Os::Windows, "box", true),
        None
    );
    for os in [Os::Linux, Os::MacOs] {
        assert_eq!(
            runner_service::local_runner_refusal(os, "box", false),
            None,
            "{os:?}: the service runs as the user and reaches the user's broker"
        );
    }
}

#[test]
fn a_missing_foreign_or_login_started_runner_is_refused_a_service() {
    let missing = runner_service::runner_dir_refusal("box", false, true, "alex").unwrap();
    assert!(missing.contains("no runner box"), "{missing}");
    let foreign = runner_service::runner_dir_refusal("box", true, false, "alex").unwrap();
    assert!(
        foreign.contains("not owned by alex") && foreign.contains("--state-dir"),
        "{foreign}"
    );
    assert_eq!(
        runner_service::runner_dir_refusal("box", true, true, "alex"),
        None
    );
    let twice = runner_service::login_entry_refusal("box", true, "alex").unwrap();
    assert!(
        twice.contains("localrunner autostart off runner box") && twice.contains("alex"),
        "{twice}"
    );
    assert_eq!(
        runner_service::login_entry_refusal("box", false, "alex"),
        None
    );
}

#[test]
fn a_service_runner_the_user_cannot_reach_is_named_with_the_command_that_can() {
    let down = runner_service::unreachable_line(Os::Windows, "box", Some(true), "down");
    assert!(
        down.starts_with(
            "runner box runs as the OS service AlLocalRunner-box: down does not reach it"
        ),
        "{down}"
    );
    assert!(down.ends_with("Stop-Service AlLocalRunner-box"), "{down}");
    for what in ["reload", "upgrade"] {
        let line = runner_service::unreachable_line(Os::Windows, "box", None, what);
        assert!(
            line.ends_with("Restart-Service AlLocalRunner-box"),
            "{line}"
        );
        assert!(
            line.contains(&format!("{what} does not reach it")),
            "{line}"
        );
    }
    let linux = runner_service::unreachable_line(Os::Linux, "box", Some(true), "down");
    assert!(
        linux.ends_with("sudo systemctl stop al-localrunner-runner-box"),
        "{linux}"
    );
    let stopped = runner_service::unreachable_line(Os::Linux, "box", Some(false), "down");
    assert!(
        stopped.contains("is stopped")
            && stopped.ends_with("sudo systemctl restart al-localrunner-runner-box"),
        "{stopped}"
    );
}

#[test]
fn up_and_upgrade_leave_a_runner_service_to_its_manager_and_say_how_to_restart_it() {
    use lr_cli::commands::up_cmd::service_runner_line;
    assert_eq!(
        service_runner_line(Os::Windows, "box", Some(true)),
        "runner box: runs as the OS service AlLocalRunner-box"
    );
    for running in [Some(false), None] {
        let line = service_runner_line(Os::Linux, "box", running);
        assert!(
            line.contains("does not start a second copy")
                && line.ends_with("sudo systemctl restart al-localrunner-runner-box"),
            "{line}"
        );
    }
    let upgrade = runner_service::upgrade_line(Os::MacOs, "box");
    assert!(
        upgrade.contains("does not restart the service")
            && upgrade
                .ends_with("sudo launchctl kickstart -k system/com.al-localrunner.runner-box"),
        "{upgrade}"
    );
}

#[test]
fn graphics_and_ci_runners_are_refused_a_service() {
    let none: Vec<String> = Vec::new();
    let refused = runner_service::refusal("ci", &none).unwrap();
    assert!(refused.contains("`ci`"), "{refused}");
    for marker in [
        "emulator",
        "android-emulator",
        "gui",
        "ios_simulator",
        "graphics",
        "desktop",
        "display",
    ] {
        let refused = runner_service::refusal("box", &[marker.to_string()]).unwrap();
        assert!(refused.contains(marker), "{refused}");
    }
    for fine in ["cargo", "gradle", "guide", "linux", "docker"] {
        assert_eq!(
            runner_service::refusal("box", &[fine.to_string()]),
            None,
            "{fine}"
        );
    }
    assert_eq!(runner_service::refusal("ci-2", &none), None);
}
