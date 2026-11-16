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
            if let Action::Run { tolerate, .. } = a {
                assert_ne!(tolerate, &Tolerate::Nothing, "{os:?}: {a}");
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
                let auto = runs
                    .iter()
                    .position(|r| r == "sc.exe config AlLocalRunner-box start= auto")
                    .unwrap();
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
        let plan = runner_service::uninstall(&runner_service(os, "box"));
        let text: Vec<String> = plan.iter().map(|a| a.to_string()).collect();
        let all = text.join("\n");
        assert!(!all.contains("Broker") && !all.contains("broker"), "{all}");
        assert!(
            !plan.iter().any(|a| matches!(a, Action::RemoveDirAll(_))),
            "{os:?}: the runner's state stays"
        );
        for a in &plan {
            if let Action::Run { tolerate, .. } = a {
                assert_ne!(tolerate, &Tolerate::Nothing, "{os:?}: {a}");
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
            runner_service::uninstall(&runner_service(os, "gpu"))
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

fn at(runs: &[String], line: &str) -> usize {
    runs.iter()
        .position(|r| r == line)
        .unwrap_or_else(|| panic!("{line} missing in {runs:#?}"))
}

#[test]
fn windows_runner_service_hands_the_runner_key_to_the_service_account_before_it_starts() {
    let i = runner_service(Os::Windows, "box");
    let runs = runs(&runner_service::install(&i));
    let key = key_of(&i);
    let create = runs
        .iter()
        .position(|r| r.starts_with("sc.exe create"))
        .unwrap();
    let owner = at(
        &runs,
        &format!(r"icacls {key} /setowner NT SERVICE\AlLocalRunner-box"),
    );
    let grant = at(
        &runs,
        &format!(r"icacls {key} /grant:r NT SERVICE\AlLocalRunner-box:(F)"),
    );
    let user_out = at(&runs, &format!("icacls {key} /remove alex"));
    let auto = at(&runs, "sc.exe config AlLocalRunner-box start= auto");
    let start = at(&runs, "sc.exe start AlLocalRunner-box");
    assert!(
        create < owner && owner < grant && grant < user_out && user_out < auto && auto < start,
        "the account exists before it owns the key, and the key is its own before the start: {runs:#?}"
    );
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
    let plan = runner_service::uninstall(&i);
    let runs = runs(&plan);
    let key = key_of(&i);
    let stop = at(&runs, "sc.exe stop AlLocalRunner-box");
    let owner = at(&runs, &format!("icacls {key} /setowner alex"));
    let grant = at(&runs, &format!("icacls {key} /grant:r alex:(F)"));
    let service_out = at(
        &runs,
        &format!(r"icacls {key} /remove NT SERVICE\AlLocalRunner-box"),
    );
    let delete = at(&runs, "sc.exe delete AlLocalRunner-box");
    assert!(
        stop < owner && owner < grant && grant < service_out && service_out < delete,
        "{runs:#?}"
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
