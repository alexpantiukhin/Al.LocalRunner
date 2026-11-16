//! A release from the network or from local files, verified and unpacked: the part of `upgrade`
//! that decides whether a downloaded binary may be started.

use super::archive::extract;
use super::fetch::{asset_name, asset_url, valid_version, Fetch};
use super::{check_sha256, sums_entry, verify_signature, VerifyError, NAMESPACE};
use std::path::{Path, PathBuf};

const SUMS: &str = "SHA256SUMS";
const SIG: &str = "SHA256SUMS.sig";

fn binary() -> &'static str {
    if cfg!(windows) {
        "localrunner.exe"
    } else {
        "localrunner"
    }
}

fn text(bytes: Vec<u8>, what: &str) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| format!("{what} is not text"))
}

fn verified(
    keys: &[String],
    sums: &[u8],
    sig: &str,
    archive: &[u8],
    archive_name: &str,
    into: &Path,
) -> Result<PathBuf, String> {
    verify_signature(keys, NAMESPACE, sums, sig).map_err(|e| e.to_string())?;
    let sums = text(sums.to_vec(), SUMS)?;
    let expected = sums_entry(&sums, archive_name)
        .ok_or_else(|| format!("{}: {archive_name}", VerifyError::MissingEntry))?;
    check_sha256(archive, expected).map_err(|e| format!("{archive_name}: {e}"))?;
    extract(archive, archive_name, into, binary())
}

/// Downloads `SHA256SUMS`, its signature and the archive of release `version` for this platform
/// into memory, checks the signature against `keys` and the archive against its line of
/// `SHA256SUMS`, and only then unpacks the binary into `into`; the path of the binary is
/// returned. Nothing is written for a release that does not verify. A build without keys refuses
/// before it downloads anything.
pub fn fetch_release(
    fetch: &dyn Fetch,
    keys: &[String],
    version: &str,
    into: &Path,
) -> Result<PathBuf, String> {
    if keys.iter().all(|k| k.trim().is_empty()) {
        return Err(VerifyError::NoKeys.to_string());
    }
    if !valid_version(version) {
        return Err(format!("`{version}` is not a version: use X.Y.Z"));
    }
    let name = asset_name();
    let sums = fetch.get(&asset_url(version, SUMS))?;
    let sig = text(fetch.get(&asset_url(version, SIG))?, SIG)?;
    let archive = fetch.get(&asset_url(version, name))?;
    verified(keys, &sums, &sig, &archive, name, into)
}

/// [`fetch_release`] from local files: `source` is the archive, or a folder that holds the
/// archive of this platform; `SHA256SUMS` and `SHA256SUMS.sig` must lie next to it. The three
/// files are read once, so changing them afterwards changes nothing.
pub fn from_offline(keys: &[String], source: &Path, into: &Path) -> Result<PathBuf, String> {
    if keys.iter().all(|k| k.trim().is_empty()) {
        return Err(VerifyError::NoKeys.to_string());
    }
    let archive_path = match source.is_dir() {
        true => source.join(asset_name()),
        false => source.to_path_buf(),
    };
    let dir = archive_path.parent().unwrap_or_else(|| Path::new("."));
    let name = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{}: not a file name", archive_path.display()))?
        .to_string();
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let archive = read(&archive_path)?;
    let sums = read(&dir.join(SUMS))?;
    let sig = text(read(&dir.join(SIG))?, SIG)?;
    verified(keys, &sums, &sig, &archive, &name, into)
}
