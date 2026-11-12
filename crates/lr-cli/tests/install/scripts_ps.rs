use crate::common::TempDir;
use crate::scripts::first_powershell;
use crate::scripts_run::{script, sign, sums_line, text};
use std::path::Path;
use std::process::Command;

pub fn powershell(exe: &str, body: &str) -> String {
    let path = script("install.ps1")
        .display()
        .to_string()
        .replace('\'', "''");
    let full = format!(
        ". '{path}'; try {{ {body}; 'RESULT:ok' }} catch {{ 'RESULT:' + $_.Exception.Message }}"
    );
    let out = Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", &full])
        .env("LOCALRUNNER_INSTALL_SOURCED", "1")
        .output()
        .expect("PowerShell runs");
    let all = text(&out);
    all.lines()
        .find_map(|l| l.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no result from {exe}: {all}"))
        .to_string()
}

pub fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[test]
fn powershell_installer_checks_sha256_before_anything_else() {
    let Some(exe) = first_powershell("powershell_installer_checks_sha256_before_anything_else")
    else {
        return;
    };
    let tmp = TempDir::new("install-ps-hash");
    let zip = tmp.path().join("al-localrunner-windows-x64.zip");
    std::fs::write(&zip, b"archive").unwrap();
    let sums = tmp.path().join("SHA256SUMS");
    let check = format!(
        "Test-ReleaseChecksum -Sums {} -File {} -Name 'al-localrunner-windows-x64.zip'",
        ps_quote(&sums),
        ps_quote(&zip)
    );
    std::fs::write(
        &sums,
        sums_line(&"0".repeat(64), "al-localrunner-windows-x64.zip"),
    )
    .unwrap();
    let wrong = powershell(exe, &check);
    assert!(wrong.contains("SHA-256"), "{wrong}");
    let matching = format!(
        "$h = (Get-FileHash -Algorithm SHA256 -LiteralPath {}).Hash.ToLowerInvariant(); \
         [IO.File]::WriteAllText({}, $h + '  al-localrunner-windows-x64.zip' + [char]10); {check}",
        ps_quote(&zip),
        ps_quote(&sums)
    );
    assert_eq!(powershell(exe, &matching), "ok");
    std::fs::write(&sums, "").unwrap();
    let missing = powershell(exe, &check);
    assert!(missing.contains("not listed"), "{missing}");
}

#[test]
fn powershell_installer_refuses_an_administrator_without_the_flag() {
    let Some(exe) =
        first_powershell("powershell_installer_refuses_an_administrator_without_the_flag")
    else {
        return;
    };
    let out = powershell(
        exe,
        "function Test-Elevated { $true }; Install-LocalRunner -Arguments @('--offline', 'none.zip')",
    );
    assert!(out.contains("administrator"), "{out}");
    let out = powershell(
        exe,
        "function Test-Elevated { $true }; Install-LocalRunner -Arguments @('--offline', 'none.zip', '--allow-elevated')",
    );
    assert!(!out.contains("administrator"), "{out}");
    let out = powershell(
        exe,
        "Install-LocalRunner -Arguments @('--role', 'everything')",
    );
    assert!(out.contains("--role"), "{out}");
}

#[test]
fn powershell_installer_verifies_the_signature_with_the_release_key() {
    let Some(exe) =
        first_powershell("powershell_installer_verifies_the_signature_with_the_release_key")
    else {
        return;
    };
    if !cfg!(windows) {
        crate::skipped(
            "powershell_installer_verifies_the_signature_with_the_release_key",
            "windows",
            "the installer runs ssh-keygen through Start-Process, which is Windows-only",
        );
        return;
    }
    let tmp = TempDir::new("install-ps-sig");
    let sums = tmp.path().join("SHA256SUMS");
    std::fs::write(&sums, sums_line(&"0".repeat(64), "x.zip")).unwrap();
    let sig = sums.with_file_name("SHA256SUMS.sig");
    let check = format!(
        "Test-ReleaseSignature -Sums {} -Signature {} -Work {}",
        ps_quote(&sums),
        ps_quote(&sig),
        ps_quote(tmp.path())
    );
    let placeholder = powershell(exe, &check);
    assert!(placeholder.contains("release key"), "{placeholder}");
    let key = match sign(tmp.path(), &sums) {
        Ok(key) => key,
        Err(why) => {
            return crate::skipped(
                "powershell_installer_verifies_the_signature_with_the_release_key",
                "ssh-keygen",
                &why,
            )
        }
    };
    let with_key = format!("$LocalRunnerReleaseKey = '{key}'; {check}");
    assert_eq!(powershell(exe, &with_key), "ok");
    std::fs::write(&sums, sums_line(&"1".repeat(64), "x.zip")).unwrap();
    let tampered = powershell(exe, &with_key);
    assert!(tampered.contains("signature"), "{tampered}");
}

fn fake_program(dir: &Path, name: &str, windows: &str, unix: &str) -> std::path::PathBuf {
    if cfg!(windows) {
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(&path, windows.replace('\n', "\r\n")).unwrap();
        path
    } else {
        let path = dir.join(name);
        std::fs::write(&path, unix).unwrap();
        let chmod = Command::new("chmod")
            .arg("755")
            .arg(&path)
            .status()
            .unwrap();
        assert!(chmod.success());
        path
    }
}

#[test]
fn powershell_installer_checks_and_unpacks_private_copies_of_offline_files() {
    let Some(exe) =
        first_powershell("powershell_installer_checks_and_unpacks_private_copies_of_offline_files")
    else {
        return;
    };
    let tmp = TempDir::new("install-ps-copy");
    let zip = tmp.path().join("al-localrunner-windows-x64.zip");
    for file in [
        &zip,
        &tmp.path().join("SHA256SUMS"),
        &tmp.path().join("SHA256SUMS.sig"),
    ] {
        std::fs::write(file, b"x").unwrap();
    }
    let body = format!(
        "$global:seen = @(); function Test-Elevated {{ $false }}; \
         function Test-ReleaseSignature {{ param($Sums, $Signature, $Work) $global:seen += $Sums, $Signature }}; \
         function Test-ReleaseChecksum {{ param($Sums, $File, $Name) $global:seen += $Sums, $File }}; \
         function Expand-Archive {{ param($LiteralPath, $DestinationPath) $global:seen += $LiteralPath; throw 'stop' }}; \
         try {{ Install-LocalRunner -Arguments @('--offline', {}) }} catch {{ }}; throw ($global:seen -join '|')",
        ps_quote(&zip)
    );
    let seen = powershell(exe, &body);
    let paths: Vec<&str> = seen.split('|').collect();
    assert_eq!(paths.len(), 5, "{seen}");
    let original = tmp.path().display().to_string();
    for p in paths {
        assert!(!p.starts_with(&original), "used the original file {p}");
        assert!(p.contains("al-localrunner-"), "{p}");
    }
}

#[test]
fn powershell_installer_runs_doctor_after_install_and_keeps_the_installation() {
    let Some(exe) = first_powershell(
        "powershell_installer_runs_doctor_after_install_and_keeps_the_installation",
    ) else {
        return;
    };
    let tmp = TempDir::new("install-ps-doctor");
    let marker = tmp.path().join("marker");
    let fake = fake_program(
        tmp.path(),
        "localrunner",
        "@echo %*>>\"%LR_MARKER%\"\n@if \"%1\"==\"doctor\" exit /b 1\n@exit /b 0\n",
        "#!/bin/sh\necho \"$@\" >> \"$LR_MARKER\"\nif [ \"$1\" = doctor ]; then exit 1; fi\n",
    );
    let body = format!(
        "$env:LR_MARKER = {}; Invoke-LocalRunnerInstall -Exe {} -Opts (Read-InstallArgs @('--role', 'runner', '--dir', '-odd'))",
        ps_quote(&marker),
        ps_quote(&fake)
    );
    assert_eq!(powershell(exe, &body), "ok");
    let lines = std::fs::read_to_string(&marker).unwrap();
    let lines: Vec<&str> = lines.lines().map(str::trim).collect();
    assert_eq!(lines, ["install --role runner --dir=-odd", "doctor"]);
}

#[test]
fn powershell_installer_reads_only_dashed_flags_and_exact_versions() {
    let Some(exe) =
        first_powershell("powershell_installer_reads_only_dashed_flags_and_exact_versions")
    else {
        return;
    };
    let bare = powershell(exe, "Read-InstallArgs @('dir', 'x') | Out-Null");
    assert!(bare.contains("unknown argument"), "{bare}");
    let newline = powershell(
        exe,
        "Read-InstallArgs @('--version', ('1.2.3' + [char]10)) | Out-Null",
    );
    assert!(newline.contains("--version"), "{newline}");
    assert_eq!(
        powershell(
            exe,
            "Read-InstallArgs @('-Version', 'v1.2.3', '--role', 'hub') | Out-Null"
        ),
        "ok"
    );
}

#[test]
fn powershell_installer_explains_an_ssh_keygen_without_signatures() {
    let Some(exe) =
        first_powershell("powershell_installer_explains_an_ssh_keygen_without_signatures")
    else {
        return;
    };
    if !cfg!(windows) {
        crate::skipped(
            "powershell_installer_explains_an_ssh_keygen_without_signatures",
            "windows",
            "the installer runs ssh-keygen through Start-Process, which is Windows-only",
        );
        return;
    }
    let tmp = TempDir::new("install-ps-oldssh");
    let sums = tmp.path().join("SHA256SUMS");
    let sig = tmp.path().join("SHA256SUMS.sig");
    std::fs::write(&sums, "x").unwrap();
    std::fs::write(&sig, "x").unwrap();
    let old = fake_program(
        tmp.path(),
        "ssh-keygen",
        "@echo unknown option -- Y 1>&2\n@exit /b 1\n",
        "#!/bin/sh\necho 'unknown option -- Y' >&2\nexit 1\n",
    );
    let body = format!(
        "$LocalRunnerReleaseKey = 'ssh-ed25519 AAAA'; function Find-SshKeygen {{ {} }}; \
         Test-ReleaseSignature -Sums {} -Signature {} -Work {}",
        ps_quote(&old),
        ps_quote(&sums),
        ps_quote(&sig),
        ps_quote(tmp.path())
    );
    let out = powershell(exe, &body);
    assert!(out.contains("8.1"), "{out}");
}
