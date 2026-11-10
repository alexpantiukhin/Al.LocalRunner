use lr_cli::os_service::plans::{
    autostart_off, autostart_on, install_service, uninstall_service, AutostartInputs, Entry,
    ServiceInputs,
};
use lr_cli::os_service::{ensure_local_user, service_account, Action, Os, Tolerate};
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
            Action::WriteFile { path, contents } => Some((path.clone(), contents.clone())),
            _ => None,
        })
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
        assert!(plan.contains(&Action::CopyIfMissing {
            from: Path::new("/home/alex/.local/share/al-localrunner").join("policy.toml"),
            to: Path::new("/var/lib/al-localrunner").join("policy.toml"),
        }));
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
                assert!(runs.iter().any(
                    |r| r.starts_with("icacls") && r.contains(&format!("/setowner {account}"))
                ));
                let grant = runs.iter().find(|r| r.contains("/grant:r")).unwrap();
                assert!(grant.contains("/inheritance:r") && grant.contains("S-1-5-32-544"));
            }
            Os::Linux => {
                assert!(runs
                    .iter()
                    .any(|r| r.starts_with("useradd --system") && r.ends_with(account)));
                assert!(runs.iter().any(|r| r == "usermod -aG al-localrunner alex"));
                assert!(runs.iter().any(|r| r.starts_with(&format!(
                    "chown -R {account}:al-localrunner /var/lib/al-localrunner"
                ))));
                let (path, unit) = written(&plan).pop().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/etc/systemd/system/al-localrunner-broker.service")
                );
                assert!(unit.contains(&format!("User={account}")), "{unit}");
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
                let (path, daemon) = written(&plan).pop().unwrap();
                assert!(path.starts_with("/Library/LaunchDaemons"));
                assert!(daemon.contains(&format!("<string>{account}</string>")));
                assert!(runs
                    .iter()
                    .any(|r| r.starts_with("launchctl bootstrap system")));
            }
        }
    }
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
        state_dir: PathBuf::from("/home/alex/state"),
        config_dir: None,
        home: PathBuf::from("/home/alex"),
        uid: 1000,
    }
}

#[test]
fn autostart_starts_the_role_as_the_user_at_logon_and_off_removes_it() {
    let hub = autostart(Os::Windows, Entry::Hub);
    let on = runs(&autostart_on(&hub));
    assert_eq!(on.len(), 1);
    assert!(on[0].starts_with(r"reg add HKCU\Software\Microsoft\Windows\CurrentVersion\Run"));
    assert!(on[0].contains("/v Al.LocalRunner hub"), "{}", on[0]);
    assert!(
        on[0].contains("/opt/bin/localrunner hub run --background --state-dir /home/alex/state"),
        "{}",
        on[0]
    );
    let off = runs(&autostart_off(&hub));
    assert!(off[0].starts_with("reg delete"), "{off:?}");

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
