mod common;

use common::TempDir;
use lr_broker::policy::LocalUserEntry;
use lr_broker::LocalAdmission;
use lr_cli::commands::join_cmd::{join_code, pick_code, CodeFrom};
use lr_cli::net::NetAdmission;
use lr_cli::os_service::programs::{unix_program, windows_program};
use lr_cli::os_service::quote::{systemd_command_line, windows_arg};
use lr_cli::os_service::trust::{check_children, facts, judge, trusted_principals, Facts};
use lr_cli::os_service::{choose_user, path_script, profile_line, profile_without};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn trusted() -> Vec<String> {
    vec!["S-1-5-32-544".into(), "S-1-5-80-9".into()]
}

fn fine() -> Facts {
    Facts {
        link: false,
        owner: Some("S-1-5-80-9".into()),
        writers: Some(vec!["S-1-5-32-544".into()]),
        hard_links: 1,
    }
}

#[test]
fn a_directory_someone_else_controls_is_refused() {
    let p = Path::new("dir");
    assert_eq!(judge(p, &fine(), &trusted()), Ok(()));
    let link = Facts {
        link: true,
        ..fine()
    };
    assert!(judge(p, &link, &trusted()).unwrap_err().contains("link"));
    let foreign_owner = Facts {
        owner: Some("S-1-5-21-1-1001".into()),
        ..fine()
    };
    assert!(judge(p, &foreign_owner, &trusted())
        .unwrap_err()
        .contains("owned by S-1-5-21-1-1001"));
    let foreign_writer = Facts {
        writers: Some(vec!["S-1-5-32-544".into(), "S-1-5-21-1-1001".into()]),
        ..fine()
    };
    assert!(judge(p, &foreign_writer, &trusted())
        .unwrap_err()
        .contains("grants access to S-1-5-21-1-1001"));
    let open = Facts {
        writers: None,
        ..fine()
    };
    assert!(judge(p, &open, &trusted()).is_err());
    let group = Facts {
        owner: Some("0".into()),
        writers: Some(vec!["group".into()]),
        link: false,
        hard_links: 1,
    };
    assert!(judge(p, &group, &["0".to_string()]).is_err());
    let linked = Facts {
        hard_links: 2,
        ..fine()
    };
    assert!(judge(p, &linked, &trusted())
        .unwrap_err()
        .contains("2 hard links"));
}

#[test]
fn a_file_with_a_second_name_is_refused() {
    let dir = TempDir::new("trust-hardlink");
    let trusted = trusted_principals("no-such-service-account-lr");
    let file = dir.path().join("policy.toml");
    std::fs::write(&file, "").unwrap();
    check_children(dir.path(), &trusted).unwrap();
    let elsewhere = TempDir::new("trust-hardlink-elsewhere");
    std::fs::hard_link(&file, elsewhere.path().join("other-name")).unwrap();
    assert!(check_children(dir.path(), &trusted)
        .unwrap_err()
        .contains("hard links"));
}

#[test]
fn a_tree_made_by_this_user_passes_its_own_check() {
    let dir = TempDir::new("trust");
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("policy.toml"), "").unwrap();
    let trusted = trusted_principals("no-such-service-account-lr");
    judge(dir.path(), &facts(dir.path()).unwrap(), &trusted).unwrap();
    check_children(dir.path(), &trusted).unwrap();
    let strangers = vec!["S-1-5-32-544".to_string(), "0".to_string()];
    assert!(
        check_children(dir.path(), &strangers).is_err() || lr_cli::host::is_elevated(),
        "files of this user pass a check that does not trust this user"
    );
}

#[cfg(unix)]
#[test]
fn unix_group_or_world_writable_and_links_are_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("trust-unix");
    let trusted = trusted_principals("no-such-service-account-lr");
    let open = dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(judge(&open, &facts(&open).unwrap(), &trusted).is_err());
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/etc", dir.path().join("link")).unwrap();
    assert!(check_children(dir.path(), &trusted)
        .unwrap_err()
        .contains("link"));
}

#[test]
fn system_programs_have_absolute_paths() {
    let system = Path::new(r"C:\Windows\System32");
    assert_eq!(windows_program(system, "sc.exe"), system.join("sc.exe"));
    assert_eq!(windows_program(system, "icacls"), system.join("icacls.exe"));
    assert_eq!(
        windows_program(system, "powershell"),
        system
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    );
    let found = unix_program("systemctl", |p| p == Path::new("/bin/systemctl")).unwrap();
    assert_eq!(found, PathBuf::from("/bin/systemctl"));
    assert!(unix_program("nothing", |_| false).is_err());
    let real =
        lr_cli::os_service::programs::program_path(if cfg!(windows) { "sc.exe" } else { "sh" })
            .unwrap();
    assert!(real.is_absolute() && real.is_file(), "{}", real.display());
    let exe_dir = std::env::current_exe().unwrap();
    assert!(!real.starts_with(exe_dir.parent().unwrap()));
}

#[test]
fn command_lines_keep_every_argument_literal() {
    assert_eq!(windows_arg("plain"), "plain");
    assert_eq!(windows_arg(""), "\"\"");
    assert_eq!(windows_arg(r"C:\a b\"), r#""C:\a b\\""#);
    assert_eq!(windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
    assert_eq!(windows_arg(r#"a\"b c"#), r#""a\\\"b c""#);
    assert_eq!(
        systemd_command_line(&["/home/%h/$X".into(), r#"a"b\c"#.into()]),
        r#""/home/%%h/$$X" "a\"b\\c""#
    );
}

#[test]
fn path_entries_keep_variables_and_come_off_again() {
    let dir = Path::new(r"C:\Users\o'neil\bin");
    let add = path_script(dir, true);
    assert!(add.contains("DoNotExpandEnvironmentNames") && add.contains("ExpandString"));
    assert!(add.contains(r"$d='C:\Users\o''neil\bin'"), "{add}");
    assert!(add.contains("-notcontains"));
    let remove = path_script(dir, false);
    assert!(remove.contains("Where-Object { $_ -ne $d }"), "{remove}");

    let home = Path::new("/home/x/it's $HOME/bin");
    let line = profile_line(home);
    assert_eq!(
        line,
        "export PATH='/home/x/it'\\''s $HOME/bin':\"$PATH\" # al-localrunner\n"
    );
    let profile = format!("alias ll='ls -l'\n{line}export EDITOR=vi\n");
    assert_eq!(
        profile_without(&profile, home),
        "alias ll='ls -l'\nexport EDITOR=vi\n"
    );
}

#[test]
fn the_installation_is_for_the_user_who_asked() {
    let me = || r"PC\admin".to_string();
    assert_eq!(choose_user(Some("alex".into()), None, me(), true), "alex");
    assert_eq!(
        choose_user(None, Some(r"PC\alex".into()), me(), true),
        r"PC\alex"
    );
    assert_eq!(
        choose_user(None, Some(r"PC\alex".into()), me(), false),
        me()
    );
    assert_eq!(choose_user(Some(String::new()), None, me(), true), me());
}

#[test]
fn the_join_code_prefers_a_file_then_the_environment_then_the_command_line() {
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        pick_code(some(" file\n"), some("env"), some("argv")),
        Some(("file".into(), CodeFrom::File))
    );
    assert_eq!(
        pick_code(some("  "), some("env"), some("argv")),
        Some(("env".into(), CodeFrom::Env))
    );
    assert_eq!(
        pick_code(None, None, some("argv")),
        Some(("argv".into(), CodeFrom::Argv))
    );
    assert_eq!(pick_code(None, Some(String::new()), None), None);
    let dir = TempDir::new("code");
    let file = dir.path().join("code.txt");
    std::fs::write(&file, "LR1-abc\r\n").unwrap();
    assert_eq!(
        join_code(Some("ignored".into()), Some(file)).unwrap(),
        "LR1-abc"
    );
}

#[test]
fn administrators_of_the_machine_own_the_broker_and_others_need_the_list() {
    let admission = NetAdmission::new(vec!["svc".into()])
        .with_admins(Arc::new(|u: &str| u.eq_ignore_ascii_case("boss")));
    let listed = vec![LocalUserEntry {
        name: "alex".into(),
        label: "dev".into(),
    }];
    let boss = admission
        .admit("boss", &[])
        .expect("an administrator is admitted");
    assert!(boss.owner);
    let service = admission.admit("svc", &[]).expect("the service account");
    assert!(service.owner);
    assert!(admission.admit("stranger", &listed).is_none());
    if let Some(alex) = admission.admit("alex", &listed) {
        assert!(!alex.owner);
        assert_eq!(alex.label, "dev");
    }
}

#[cfg(unix)]
#[test]
fn a_service_directory_that_appears_meanwhile_is_judged_not_adopted() {
    use lr_cli::os_service::trust::create_service_leaf;
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("leaf");
    let trusted = trusted_principals("no-such-service-account-lr");
    let fresh = dir.path().join("a").join("svc");
    create_service_leaf(&fresh, &trusted).unwrap();
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let raced = dir.path().join("raced");
    std::fs::create_dir(&raced).unwrap();
    std::fs::set_permissions(&raced, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(create_service_leaf(&raced, &trusted).is_err());
    let link = dir.path().join("linked");
    std::os::unix::fs::symlink("/etc", &link).unwrap();
    assert!(create_service_leaf(&link, &trusted)
        .unwrap_err()
        .contains("link"));
}

#[cfg(unix)]
#[test]
fn service_directory_contents_are_checked_after_it_is_closed() {
    use lr_cli::os_service::trust::secure_service_dir;
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("svc-after");
    let svc = dir.path().join("svc");
    std::fs::create_dir(&svc).unwrap();
    std::fs::set_permissions(&svc, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/etc", svc.join("link")).unwrap();
    let account = "no-such-service-account-lr";
    assert!(secure_service_dir(&svc, account)
        .unwrap_err()
        .contains("link"));
    let mode = std::fs::metadata(&svc).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    std::fs::remove_file(svc.join("link")).unwrap();
    secure_service_dir(&svc, account).unwrap();
    let fresh = dir.path().join("fresh");
    secure_service_dir(&fresh, account).unwrap();
    assert!(fresh.is_dir());
}
