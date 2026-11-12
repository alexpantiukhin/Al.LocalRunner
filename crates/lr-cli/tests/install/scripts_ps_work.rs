use crate::common::TempDir;
use crate::scripts::first_powershell;
use crate::scripts_ps::{powershell, ps_quote};
use crate::scripts_run::{script, text};
use std::process::Command;

const RELEASES: &str = "https://github.com/alexpantiukhin/Al.LocalRunner/releases";

const NO_NETWORK: &str = "function Test-Elevated { $false }; \
     function Get-ReleaseFile { param($Url, $OutFile) $global:net += $Url }; \
     function Invoke-WebRequest { $global:net += 'Invoke-WebRequest' }; \
     $global:net = @();";

#[test]
fn powershell_installer_closes_its_work_directory_to_the_owner_and_stays_offline() {
    let test = "powershell_installer_closes_its_work_directory_to_the_owner_and_stays_offline";
    let Some(exe) = first_powershell(test) else {
        return;
    };
    let tmp = TempDir::new("install-ps-work");
    let zip = tmp.path().join("al-localrunner-windows-x64.zip");
    for file in [&zip, &tmp.path().join("SHA256SUMS")] {
        std::fs::write(file, b"x").unwrap();
    }
    let body = format!(
        "{NO_NETWORK} \
         function Test-ReleaseSignature {{ param($Sums, $Signature, $Work) \
           if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {{ \
             $a = Get-Acl -LiteralPath $Work; \
             $who = ($a.Access | ForEach-Object {{ $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value + ':' + $_.FileSystemRights + ':' + $_.IsInherited }}) -join ','; \
             $me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value; \
             $global:work = \"$($a.AreAccessRulesProtected)|$who|$me\" \
           }} else {{ $global:work = 'unix|' + (Get-Item -LiteralPath $Work).UnixMode }}; \
           throw 'stop' }}; \
         try {{ Install-LocalRunner -Arguments @('--offline', {}) }} catch {{ }}; \
         throw ($global:work + '#' + ($global:net -join ','))",
        ps_quote(&zip)
    );
    let seen = powershell(exe, &body);
    let (work, net) = seen
        .split_once('#')
        .expect("the work directory was inspected");
    assert_eq!(net, "", "--offline made a network call: {net}");
    let parts: Vec<&str> = work.split('|').collect();
    if parts[0] == "unix" {
        assert_eq!(parts[1], "drwx------", "{work}");
    } else {
        assert_eq!(parts.len(), 3, "{work}");
        assert_eq!(parts[0], "True", "inheritance is cut: {work}");
        assert_eq!(
            parts[1],
            format!("{}:FullControl:False", parts[2]),
            "only the owner has access: {work}"
        );
    }
}

#[test]
fn powershell_installer_downloads_the_release_files_over_https_only() {
    let test = "powershell_installer_downloads_the_release_files_over_https_only";
    let Some(exe) = first_powershell(test) else {
        return;
    };
    let fetched = |args: &str| {
        powershell(
            exe,
            &format!(
                "{NO_NETWORK} function Get-ReleaseTarget {{ $LocalRunnerArchive }}; \
                 function Test-ReleaseSignature {{ throw 'stop' }}; \
                 try {{ Install-LocalRunner -Arguments @({args}) }} catch {{ }}; \
                 throw ($global:net -join ',')"
            ),
        )
    };
    let files = [
        "SHA256SUMS",
        "SHA256SUMS.sig",
        "al-localrunner-windows-x64.zip",
    ];
    let latest: Vec<String> = files
        .iter()
        .map(|f| format!("{RELEASES}/latest/download/{f}"))
        .collect();
    assert_eq!(fetched("'--role', 'runner'"), latest.join(","));
    let pinned: Vec<String> = files
        .iter()
        .map(|f| format!("{RELEASES}/download/v1.2.3/{f}"))
        .collect();
    assert_eq!(fetched("'--version', '1.2.3'"), pinned.join(","));
}

#[test]
fn powershell_installer_warns_when_sourced_for_tests_and_installs_nothing() {
    let test = "powershell_installer_warns_when_sourced_for_tests_and_installs_nothing";
    let Some(exe) = first_powershell(test) else {
        return;
    };
    let path = ps_quote(&script("install.ps1"));
    let out = Command::new(exe)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(". {path} '--role' 'everything'; 'DONE'"),
        ])
        .env("LOCALRUNNER_INSTALL_SOURCED", "1")
        .output()
        .expect("PowerShell runs");
    let all = text(&out);
    assert!(
        all.contains(
            "LOCALRUNNER_INSTALL_SOURCED=1 is set: functions loaded for tests, nothing installed"
        ),
        "{all}"
    );
    assert!(
        !all.contains("install.ps1: --role"),
        "the main part read the arguments: {all}"
    );
    assert!(all.contains("DONE"), "{all}");
}
