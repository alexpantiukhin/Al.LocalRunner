use crate::{install_file, repo_root};
use std::process::Command;

const RELEASES: &str = "https://github.com/alexpantiukhin/Al.LocalRunner/releases";
const NAMESPACE: &str = "al-localrunner-release";
pub const KEY_PLACEHOLDER: &str = "@RELEASE_PUBKEY@";

fn body_of(text: &str, open: &str) -> String {
    let start = text
        .find(open)
        .unwrap_or_else(|| panic!("`{open}` is defined"));
    let rest = &text[start..];
    let end = rest.find("\n}\n").expect("the definition ends") + 1;
    rest[..end].to_string()
}

fn assert_in_order(body: &str, steps: &[&str]) {
    let mut last = 0;
    for step in steps {
        let at = body[last..]
            .find(step)
            .unwrap_or_else(|| panic!("`{step}` comes after the previous step"));
        last += at + step.len();
    }
}

fn assert_only_release_urls(name: &str, text: &str) {
    for (at, _) in text.match_indices("://") {
        let start = text[..at]
            .rfind(|c: char| !c.is_ascii_alphanumeric())
            .map_or(0, |p| p + 1);
        assert!(
            text[start..].starts_with(RELEASES),
            "{name} names a network address other than the releases: {}",
            &text[start..(at + 40).min(text.len())]
        );
    }
}

#[test]
fn shell_installer_has_the_flags_checks_and_order_of_the_spec() {
    let text = install_file("install.sh");
    assert!(text.starts_with("#!/bin/sh\n"), "POSIX sh");
    assert!(text.contains("\nset -eu\n"));
    for flag in [
        "--version)",
        "--dir)",
        "--offline)",
        "--role)",
        "--allow-elevated)",
    ] {
        assert!(text.contains(flag), "flag {flag}");
    }
    assert!(text.contains("hub | broker | runner | all)"), "role values");
    assert!(text.contains("\"$(id -u)\" = 0"), "root check");
    assert!(text.contains("ssh-keygen -Y verify"), "signature check");
    assert!(text.contains("SHA256SUMS.sig"));
    assert!(text.contains(&format!("RELEASE_KEY=\"{KEY_PLACEHOLDER}\"")));
    assert!(text.contains(&format!("SIG_NAMESPACE=\"{NAMESPACE}\"")));
    assert!(text.contains("--proto '=https'"), "curl only over https");
    for banned in [
        "| sh",
        "| bash",
        "eval ",
        "[[",
        "function ",
        "local ",
        "source ",
        "==",
    ] {
        assert!(!text.contains(banned), "install.sh contains `{banned}`");
    }
    assert_only_release_urls("install.sh", &text);
    let main = body_of(&text, "main() {");
    assert_in_order(
        &main,
        &[
            "parse_args \"$@\"",
            "check_user",
            "verify_signature",
            "verify_checksum",
            "extract",
            "run_install",
        ],
    );
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap();
    assert_eq!(last, "fi", "main runs last, after the whole script arrived");
    assert!(text.contains("[ \"${LOCALRUNNER_INSTALL_SOURCED:-}\" != 1 ]"));
}

#[test]
fn powershell_installer_has_the_flags_checks_and_order_of_the_spec() {
    let text = install_file("install.ps1");
    for flag in [
        "'version'",
        "'dir'",
        "'offline'",
        "'role'",
        "'allow-elevated'",
    ] {
        assert!(text.contains(flag), "flag {flag}");
    }
    assert!(
        text.contains("'hub', 'broker', 'runner', 'all'"),
        "role values"
    );
    assert!(text.contains("[Security.Principal.WindowsBuiltInRole]::Administrator"));
    assert!(text.contains("'-Y', 'verify'"), "signature check");
    assert!(text.contains("Get-FileHash"));
    assert!(text.contains("SHA256SUMS.sig"));
    assert!(text.contains(&format!("$LocalRunnerReleaseKey = '{KEY_PLACEHOLDER}'")));
    assert!(text.contains(&format!("$LocalRunnerSigNamespace = '{NAMESPACE}'")));
    for line in text.lines() {
        let code = line.split('#').next().unwrap().trim().to_ascii_lowercase();
        assert!(
            !(code == "exit" || code.starts_with("exit ")),
            "exit would close the shell of `irm | iex`: {line}"
        );
        for banned in ["invoke-expression", "iex ", "&&", "||", "??", "?.", "| iex"] {
            assert!(
                !code.contains(banned),
                "install.ps1 contains `{banned}`: {line}"
            );
        }
    }
    assert_only_release_urls("install.ps1", &text);
    let main = body_of(&text, "function Install-LocalRunner {");
    assert_in_order(
        &main,
        &[
            "Read-InstallArgs",
            "Test-Elevated",
            "Test-ReleaseSignature",
            "Test-ReleaseChecksum",
            "Expand-Archive",
            "'install'",
        ],
    );
    assert!(text.contains("if ($env:LOCALRUNNER_INSTALL_SOURCED -ne '1')"));
}

#[test]
fn both_installers_fetch_the_same_release_files() {
    let sh = install_file("install.sh");
    let ps = install_file("install.ps1");
    for text in [&sh, &ps] {
        assert!(text.contains(RELEASES));
        assert!(text.contains("latest/download"));
        assert!(text.contains("download/v"));
        assert!(text.contains("al-localrunner-"));
    }
    assert!(sh.contains(".tar.gz"));
    assert!(ps.contains("al-localrunner-windows-x64.zip"));
}

fn powershells() -> Vec<&'static str> {
    ["powershell", "pwsh"]
        .into_iter()
        .filter(|exe| {
            Command::new(exe)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$PSVersionTable.PSVersion.Major",
                ])
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .collect()
}

pub fn first_powershell() -> Option<&'static str> {
    let found = powershells();
    let pick = found
        .iter()
        .copied()
        .find(|e| *e == "pwsh")
        .or(found.first().copied());
    if pick.is_none() {
        eprintln!("skipped: neither pwsh nor powershell is on PATH");
    }
    pick
}

#[test]
fn powershell_installer_parses_without_errors() {
    let shells = powershells();
    if shells.is_empty() {
        eprintln!("skipped: neither pwsh nor powershell is on PATH");
        return;
    }
    let path = repo_root().join("install").join("install.ps1");
    let check = format!(
        "$t = $null; $e = $null; [void][System.Management.Automation.Language.Parser]::ParseFile('{}', [ref]$t, [ref]$e); if ($e.Count -gt 0) {{ $e | ForEach-Object {{ $_.ToString() }}; exit 1 }}",
        path.display().to_string().replace('\'', "''")
    );
    for exe in shells {
        let out = Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Command", &check])
            .output()
            .expect("PowerShell runs");
        assert!(
            out.status.success(),
            "{exe} parse errors:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
