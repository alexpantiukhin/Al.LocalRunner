use crate::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NAMESPACE: &str = "al-localrunner-release";

pub fn script(name: &str) -> PathBuf {
    repo_root().join("install").join(name)
}

pub fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

pub fn sign(dir: &Path, file: &Path) -> Option<String> {
    let key = dir.join("release-key");
    let made = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
        .arg(&key)
        .output();
    if !made.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: ssh-keygen cannot create a key here");
        return None;
    }
    let signed = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-f"])
        .arg(&key)
        .args(["-n", NAMESPACE])
        .arg(file)
        .output();
    if !signed.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: ssh-keygen cannot sign (OpenSSH 8.0+ needed)");
        return None;
    }
    Some(
        std::fs::read_to_string(key.with_extension("pub"))
            .unwrap()
            .trim()
            .to_string(),
    )
}

pub fn sums_line(hash: &str, name: &str) -> String {
    format!("{hash}  {name}\n")
}

#[cfg(unix)]
mod sh {
    use super::*;
    use crate::common::TempDir;
    use crate::scripts::KEY_PLACEHOLDER;
    use std::os::unix::fs::PermissionsExt;

    const ARCHIVE: &str = "al-localrunner-linux-x64.tar.gz";

    struct Fixture {
        _tmp: TempDir,
        root: PathBuf,
        archive: PathBuf,
        sums: PathBuf,
        key: Option<String>,
    }

    fn executable(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn sha256(file: &Path) -> String {
        for (exe, args) in [("sha256sum", vec![]), ("shasum", vec!["-a", "256"])] {
            if let Ok(out) = Command::new(exe).args(&args).arg(file).output() {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout);
                    return s.split_whitespace().next().unwrap().to_string();
                }
            }
        }
        panic!("no sha256sum or shasum");
    }

    fn fixture(uid: &str, wrong_hash: bool) -> Fixture {
        let tmp = TempDir::new("install-sh");
        let root = tmp.path().to_path_buf();
        executable(&root.join("stub/id"), &format!("#!/bin/sh\necho {uid}\n"));
        executable(
            &root.join("pkg/localrunner"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$LR_MARKER\"\nif [ \"$1\" = doctor ]; then exit 1; fi\n",
        );
        let release = root.join("release");
        std::fs::create_dir_all(&release).unwrap();
        let archive = release.join(ARCHIVE);
        let tar = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root.join("pkg"))
            .arg("localrunner")
            .status()
            .unwrap();
        assert!(tar.success());
        let hash = if wrong_hash {
            "0".repeat(64)
        } else {
            sha256(&archive)
        };
        let sums = release.join("SHA256SUMS");
        std::fs::write(&sums, sums_line(&hash, ARCHIVE)).unwrap();
        let key = sign(&root, &sums);
        Fixture {
            _tmp: tmp,
            root,
            archive,
            sums,
            key,
        }
    }

    impl Fixture {
        fn marker(&self) -> PathBuf {
            self.root.join("marker")
        }

        fn command(&self) -> Command {
            let mut cmd = Command::new("sh");
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env(
                "PATH",
                format!("{}:{path}", self.root.join("stub").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("LR_MARKER", self.marker());
            cmd
        }

        fn run_direct(&self, args: &[&str]) -> Output {
            self.command()
                .arg(script("install.sh"))
                .args(args)
                .output()
                .unwrap()
        }

        fn run_with_key(&self, args: &[&str]) -> Output {
            self.command()
                .env("LOCALRUNNER_INSTALL_SOURCED", "1")
                .env("LR_KEY", self.key.as_deref().unwrap())
                .arg("-c")
                .arg(". \"$0\"; RELEASE_KEY=$LR_KEY; main \"$@\"")
                .arg(script("install.sh"))
                .args(args)
                .output()
                .unwrap()
        }

        fn offline(&self) -> String {
            self.archive.display().to_string()
        }

        fn installed(&self) -> Option<String> {
            std::fs::read_to_string(self.marker()).ok()
        }
    }

    #[test]
    fn shell_installer_is_valid_posix_sh() {
        let out = Command::new("sh")
            .arg("-n")
            .arg(script("install.sh"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
    }

    #[test]
    fn root_is_refused_unless_allowed_explicitly() {
        let f = fixture("0", false);
        let out = f.run_direct(&["--offline", &f.offline(), "--role", "runner"]);
        assert!(!out.status.success());
        assert!(text(&out).contains("root"), "{}", text(&out));
        assert!(f.installed().is_none());
        let out = f.run_direct(&["--offline", &f.offline(), "--allow-elevated"]);
        assert!(!out.status.success());
        assert!(!text(&out).contains("root"), "{}", text(&out));
        assert!(text(&out).contains("release key"), "{}", text(&out));
    }

    #[test]
    fn a_script_without_the_release_key_installs_nothing() {
        let f = fixture("1000", false);
        let source = std::fs::read_to_string(script("install.sh")).unwrap();
        assert!(source.contains(KEY_PLACEHOLDER));
        let out = f.run_direct(&["--offline", &f.offline()]);
        assert!(!out.status.success());
        assert!(text(&out).contains("release key"), "{}", text(&out));
        assert!(f.installed().is_none());
    }

    #[test]
    fn wrong_sha256_aborts_before_install() {
        let f = fixture("1000", true);
        if f.key.is_none() {
            return;
        }
        let dir = f.root.join("bin").display().to_string();
        let out = f.run_with_key(&["--offline", &f.offline(), "--dir", &dir]);
        assert!(!out.status.success());
        assert!(text(&out).contains("SHA-256"), "{}", text(&out));
        assert!(f.installed().is_none(), "the binary never ran");
        assert!(!f.root.join("bin").exists());
    }

    #[test]
    fn tampered_checksums_fail_the_signature_check() {
        let f = fixture("1000", false);
        if f.key.is_none() {
            return;
        }
        let mut sums = std::fs::read_to_string(&f.sums).unwrap();
        sums.push_str(&sums_line(&"1".repeat(64), "other.tar.gz"));
        std::fs::write(&f.sums, sums).unwrap();
        let out = f.run_with_key(&["--offline", &f.offline()]);
        assert!(!out.status.success());
        assert!(text(&out).contains("signature"), "{}", text(&out));
        assert!(f.installed().is_none());
    }

    #[test]
    fn an_archive_missing_from_the_checksums_is_refused() {
        let f = fixture("1000", false);
        if f.key.is_none() {
            return;
        }
        let renamed = f
            .archive
            .with_file_name("al-localrunner-linux-arm64.tar.gz");
        std::fs::rename(&f.archive, &renamed).unwrap();
        let out = f.run_with_key(&["--offline", &renamed.display().to_string()]);
        assert!(!out.status.success());
        assert!(text(&out).contains("not listed"), "{}", text(&out));
        assert!(f.installed().is_none());
    }

    #[test]
    fn a_verified_archive_runs_the_binary_install_and_a_rerun_does_the_same() {
        let f = fixture("1000", false);
        if f.key.is_none() {
            return;
        }
        let dir = f.root.join("bin").display().to_string();
        let args = ["--offline", &f.offline(), "--dir", &dir, "--role", "runner"];
        let first = f.run_with_key(&args);
        assert!(first.status.success(), "{}", text(&first));
        let once = format!("install\n--role\nrunner\n--dir={dir}\ndoctor\n");
        assert_eq!(f.installed().as_deref(), Some(once.as_str()));
        assert!(text(&first).contains("doctor"), "{}", text(&first));
        let again = f.run_with_key(&args);
        assert!(again.status.success(), "{}", text(&again));
        assert_eq!(f.installed(), Some(format!("{once}{once}")));
    }

    #[test]
    fn offline_files_are_checked_and_unpacked_as_private_copies() {
        let f = fixture("1000", false);
        if f.key.is_none() {
            return;
        }
        let real_tar = Command::new("sh")
            .args(["-c", "command -v tar"])
            .output()
            .unwrap();
        let real_tar = String::from_utf8_lossy(&real_tar.stdout).trim().to_string();
        executable(
            &f.root.join("evil/localrunner"),
            "#!/bin/sh\necho EVIL >> \"$LR_MARKER\"\n",
        );
        let evil = f.root.join("evil.tar.gz");
        let made = Command::new("tar")
            .arg("-czf")
            .arg(&evil)
            .arg("-C")
            .arg(f.root.join("evil"))
            .arg("localrunner")
            .status()
            .unwrap();
        assert!(made.success());
        executable(
            &f.root.join("stub/tar"),
            &format!(
                "#!/bin/sh\ncp '{}' '{}'\nexec '{real_tar}' \"$@\"\n",
                evil.display(),
                f.archive.display()
            ),
        );
        let out = f.run_with_key(&["--offline", &f.offline()]);
        assert!(out.status.success(), "{}", text(&out));
        let installed = f.installed().unwrap_or_default();
        assert!(!installed.contains("EVIL"), "the swapped archive ran");
        assert!(installed.starts_with("install\n"), "{installed}");
    }

    #[test]
    fn an_ssh_keygen_without_signatures_gets_a_clear_error() {
        let f = fixture("1000", false);
        if f.key.is_none() {
            return;
        }
        executable(
            &f.root.join("stub/ssh-keygen"),
            "#!/bin/sh\necho 'ssh-keygen: illegal option -- Y' >&2\nexit 1\n",
        );
        let out = f.run_with_key(&["--offline", &f.offline()]);
        assert!(!out.status.success());
        assert!(text(&out).contains("8.1"), "{}", text(&out));
        assert!(f.installed().is_none());
    }

    #[test]
    fn the_test_switch_in_the_environment_is_not_silent() {
        let f = fixture("1000", false);
        let out = f
            .command()
            .env("LOCALRUNNER_INSTALL_SOURCED", "1")
            .arg(script("install.sh"))
            .output()
            .unwrap();
        let all = text(&out);
        assert!(all.contains("LOCALRUNNER_INSTALL_SOURCED"), "{all}");
        assert!(f.installed().is_none());
    }

    #[test]
    fn bad_arguments_are_refused() {
        let f = fixture("1000", false);
        for args in [
            vec!["--role", "everything"],
            vec!["--color"],
            vec!["--version"],
            vec!["--version", "1.0;rm"],
        ] {
            let out = f.run_direct(&args);
            assert!(!out.status.success(), "{args:?}");
            assert!(f.installed().is_none());
        }
    }
}
