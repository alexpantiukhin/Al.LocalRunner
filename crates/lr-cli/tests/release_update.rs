mod common;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use common::TempDir;
use ed25519_dalek::{Signer, SigningKey};
use lr_cli::commands::upgrade_fetch::{fetch_and_relaunch, is_installed, relaunch_args};
use lr_cli::commands::UpgradeArgs;
use lr_cli::paths::Layout;
use lr_cli::release::{
    asset_name, asset_url, fetch_release, from_offline, latest_version, Fetch, NAMESPACE,
};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

struct MapFetch {
    files: HashMap<String, Vec<u8>>,
    asked: Mutex<Vec<String>>,
}

impl MapFetch {
    fn new(files: Vec<(String, Vec<u8>)>) -> MapFetch {
        MapFetch {
            files: files.into_iter().collect(),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl Fetch for MapFetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.asked.lock().unwrap().push(url.to_string());
        self.files
            .get(url)
            .cloned()
            .ok_or_else(|| format!("{url}: 404"))
    }
}

fn put(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u32).to_be_bytes());
    out.extend_from_slice(field);
}

fn key(seed: u8) -> (SigningKey, String) {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let mut blob = Vec::new();
    put(&mut blob, b"ssh-ed25519");
    put(&mut blob, signing.verifying_key().as_bytes());
    let line = format!("ssh-ed25519 {} test", STANDARD.encode(blob));
    (signing, line)
}

fn sign(signing: &SigningKey, namespace: &str, message: &[u8]) -> String {
    let mut data = b"SSHSIG".to_vec();
    put(&mut data, namespace.as_bytes());
    put(&mut data, b"");
    put(&mut data, b"sha512");
    put(&mut data, &Sha512::digest(message));
    let signature = signing.sign(&data).to_bytes();
    let mut public = Vec::new();
    put(&mut public, b"ssh-ed25519");
    put(&mut public, signing.verifying_key().as_bytes());
    let mut sig_blob = Vec::new();
    put(&mut sig_blob, b"ssh-ed25519");
    put(&mut sig_blob, &signature);
    let mut blob = b"SSHSIG".to_vec();
    blob.extend_from_slice(&1u32.to_be_bytes());
    put(&mut blob, &public);
    put(&mut blob, namespace.as_bytes());
    put(&mut blob, b"");
    put(&mut blob, b"sha512");
    put(&mut blob, &sig_blob);
    let body = STANDARD.encode(blob);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(70)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
        lines.join("\n")
    )
}

fn archive(binary: &[u8]) -> Vec<u8> {
    if cfg!(windows) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for name in ["localrunner.exe", "localrunnerw.exe"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(binary).unwrap();
        }
        zip.finish().unwrap().into_inner()
    } else {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "localrunner", binary).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }
}

fn sums_for(archive: &[u8]) -> String {
    let hash: String = Sha256::digest(archive)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hash}  {}\n", asset_name())
}

struct Release {
    fetch: MapFetch,
    keys: Vec<String>,
}

fn release(version: &str, archive_bytes: Vec<u8>, sums: String, namespace: &str) -> Release {
    let (signing, line) = key(1);
    let sig = sign(&signing, namespace, sums.as_bytes());
    Release {
        fetch: MapFetch::new(vec![
            (asset_url(version, "SHA256SUMS"), sums.into_bytes()),
            (asset_url(version, "SHA256SUMS.sig"), sig.into_bytes()),
            (asset_url(version, asset_name()), archive_bytes),
        ]),
        keys: vec![line],
    }
}

fn good(version: &str) -> Release {
    let bytes = archive(b"new binary");
    let sums = sums_for(&bytes);
    release(version, bytes, sums, NAMESPACE)
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

#[test]
fn latest_version_skips_prereleases() {
    let body = r#"[
        {"tag_name": "v0.4.0-rc1", "prerelease": true, "draft": false},
        {"tag_name": "v0.4.0", "prerelease": false, "draft": true},
        {"tag_name": "v0.3.0", "prerelease": false, "draft": false},
        {"tag_name": "v0.2.0", "prerelease": false, "draft": false}
    ]"#;
    let fetch = MapFetch::new(vec![(
        "https://api.github.com/repos/alexpantiukhin/Al.LocalRunner/releases".into(),
        body.as_bytes().to_vec(),
    )]);
    assert_eq!(latest_version(&fetch), Ok("0.3.0".to_string()));
    let none = MapFetch::new(vec![(
        "https://api.github.com/repos/alexpantiukhin/Al.LocalRunner/releases".into(),
        b"[]".to_vec(),
    )]);
    assert!(latest_version(&none).is_err());
}

#[test]
fn a_verified_release_is_unpacked() {
    let dir = TempDir::new("release_ok");
    let r = good("0.3.0");
    let binary = fetch_release(&r.fetch, &r.keys, "0.3.0", dir.path()).unwrap();
    assert_eq!(std::fs::read(&binary).unwrap(), b"new binary");
}

#[test]
fn fetch_release_installs_nothing_on_a_bad_signature() {
    let dir = TempDir::new("release_bad_sig");
    let bytes = archive(b"new binary");
    let sums = sums_for(&bytes);
    let r = release("0.3.0", bytes, sums, "another-namespace");
    let err = fetch_release(&r.fetch, &r.keys, "0.3.0", dir.path()).unwrap_err();
    assert!(err.contains("not a release signature"), "{err}");
    assert_eq!(entries(dir.path()), 0);
    let (_, other) = key(2);
    let good = good("0.3.0");
    let err = fetch_release(&good.fetch, &[other], "0.3.0", dir.path()).unwrap_err();
    assert!(err.contains("does not trust"), "{err}");
    assert_eq!(entries(dir.path()), 0);
}

#[test]
fn fetch_release_installs_nothing_on_a_sha_mismatch() {
    let dir = TempDir::new("release_bad_sha");
    let signed_for = archive(b"what was signed");
    let sums = sums_for(&signed_for);
    let r = release("0.3.0", archive(b"what was served"), sums, NAMESPACE);
    let err = fetch_release(&r.fetch, &r.keys, "0.3.0", dir.path()).unwrap_err();
    assert!(err.contains("SHA-256"), "{err}");
    assert_eq!(entries(dir.path()), 0);
}

#[test]
fn a_truncated_archive_leaves_no_binary() {
    let dir = TempDir::new("release_truncated");
    let whole = archive(b"new binary");
    let cut = whole[..whole.len() / 2].to_vec();
    let sums = sums_for(&cut);
    let r = release("0.3.0", cut, sums, NAMESPACE);
    let err = fetch_release(&r.fetch, &r.keys, "0.3.0", dir.path()).unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(entries(dir.path()), 0);
}

#[test]
fn an_empty_key_list_refuses_with_no_release_key_built_in() {
    let dir = TempDir::new("release_no_keys");
    let r = good("0.3.0");
    let err = fetch_release(&r.fetch, &[], "0.3.0", dir.path()).unwrap_err();
    assert!(err.contains("no release key"), "{err}");
    assert!(
        r.fetch.asked.lock().unwrap().is_empty(),
        "nothing is downloaded"
    );
    let err = from_offline(&[], dir.path(), dir.path()).unwrap_err();
    assert!(err.contains("no release key"), "{err}");
}

#[test]
fn an_explicit_version_may_be_a_prerelease() {
    let dir = TempDir::new("release_pre");
    let r = good("0.4.0-rc1");
    assert!(fetch_release(&r.fetch, &r.keys, "0.4.0-rc1", dir.path()).is_ok());
    assert!(r.fetch.asked.lock().unwrap()[0].contains("/download/v0.4.0-rc1/"));
    for bad in ["", "latest", "1.2", "1.2.3/../x", "1.2.3 4"] {
        let err = fetch_release(&r.fetch, &r.keys, bad, dir.path()).unwrap_err();
        assert!(err.contains("not a version"), "{bad:?}: {err}");
    }
}

#[test]
fn offline_takes_the_three_files_from_a_directory() {
    let source = TempDir::new("offline_source");
    let r = good("0.3.0");
    for (url, bytes) in &r.fetch.files {
        let name = url.rsplit('/').next().unwrap();
        std::fs::write(source.path().join(name), bytes).unwrap();
    }
    let out = TempDir::new("offline_out");
    let binary = from_offline(&r.keys, source.path(), out.path()).unwrap();
    assert_eq!(std::fs::read(&binary).unwrap(), b"new binary");
    let out2 = TempDir::new("offline_out2");
    let archive_path = source.path().join(asset_name());
    assert!(from_offline(&r.keys, &archive_path, out2.path()).is_ok());
    std::fs::write(source.path().join("SHA256SUMS.sig"), "garbage").unwrap();
    let out3 = TempDir::new("offline_out3");
    assert!(from_offline(&r.keys, source.path(), out3.path()).is_err());
    assert_eq!(entries(out3.path()), 0);
}

fn upgrade_args() -> UpgradeArgs {
    UpgradeArgs {
        dir: None,
        yes: true,
        cancel: true,
        allow_elevated: false,
        dry_run: false,
        wait_timeout: None,
        version: Some("0.3.0".into()),
        offline: None,
    }
}

#[test]
fn the_downloaded_binary_is_started_with_upgrade_and_the_remaining_flags() {
    let dir = TempDir::new("relaunch");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let r = good("0.3.0");
    let calls = Mutex::new(Vec::new());
    let spawn = |binary: &Path, argv: &[String]| {
        assert!(binary.is_file(), "the verified binary exists while it runs");
        calls.lock().unwrap().push(argv.to_vec());
        Ok(7)
    };
    let code = fetch_and_relaunch(&layout, &upgrade_args(), &r.fetch, &r.keys, &spawn);
    assert_eq!(code, Ok(7));
    let argv = calls.lock().unwrap()[0].clone();
    assert_eq!(argv[0], "--state-dir");
    assert_eq!(&argv[4..], ["upgrade", "--yes", "--cancel"]);
    let mut full = upgrade_args();
    full.wait_timeout = Some(std::time::Duration::from_secs(900));
    full.dry_run = true;
    let tail = relaunch_args(&layout, &full);
    assert_eq!(
        &tail[4..],
        [
            "upgrade",
            "--yes",
            "--cancel",
            "--dry-run",
            "--wait-timeout",
            "900s"
        ]
    );
}

#[test]
fn nothing_is_started_when_the_release_does_not_verify() {
    let dir = TempDir::new("relaunch_bad");
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    let r = good("0.3.0");
    let (_, other) = key(2);
    let started = Mutex::new(false);
    let spawn = |_: &Path, _: &[String]| {
        *started.lock().unwrap() = true;
        Ok(0)
    };
    let code = fetch_and_relaunch(&layout, &upgrade_args(), &r.fetch, &[other], &spawn);
    assert!(code.is_err());
    assert!(!*started.lock().unwrap());
}

#[test]
fn running_from_another_folder_keeps_today_s_flow() {
    let dir = TempDir::new("installed");
    let installed = dir.path().join("localrunner");
    let other = dir.path().join("new-localrunner");
    std::fs::write(&installed, "a").unwrap();
    std::fs::write(&other, "b").unwrap();
    assert!(is_installed(&installed, &installed));
    assert!(!is_installed(&other, &installed));
    assert!(!is_installed(&dir.path().join("missing"), &installed));
}
