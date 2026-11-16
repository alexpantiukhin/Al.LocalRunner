//! Unpacking a release archive: only the binaries by their plain names, never a path from the
//! archive.

use std::io::Read;
use std::path::{Path, PathBuf};

const NAMES: [&str; 3] = ["localrunner", "localrunner.exe", "localrunnerw.exe"];

fn wanted(path: &str) -> Option<&'static str> {
    NAMES.iter().copied().find(|n| *n == path)
}

fn write(into: &Path, name: &str, bytes: &[u8], written: &mut Vec<PathBuf>) -> Result<(), String> {
    let path = into.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    written.push(path.clone());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn untar(archive: &[u8], into: &Path, written: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().map_err(|e| format!("archive: {e}"))? {
        let mut entry = entry.map_err(|e| format!("archive: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(|e| format!("archive: {e}"))?;
        let Some(name) = path
            .to_str()
            .map(|p| p.trim_start_matches("./"))
            .and_then(wanted)
        else {
            continue;
        };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("archive: {e}"))?;
        write(into, name, &bytes, written)?;
    }
    Ok(())
}

fn unzip(archive: &[u8], into: &Path, written: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| format!("archive: {e}"))?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("archive: {e}"))?;
        if !file.is_file() {
            continue;
        }
        let Some(name) = wanted(file.name()) else {
            continue;
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("archive: {e}"))?;
        write(into, name, &bytes, written)?;
    }
    Ok(())
}

/// Unpacks the binaries of `archive` (`file` names its format by the extension) into `into` and
/// returns the path of `main`. Whatever was written is removed again when anything fails.
pub fn extract(archive: &[u8], file: &str, into: &Path, main: &str) -> Result<PathBuf, String> {
    let mut written = Vec::new();
    let done = std::fs::create_dir_all(into)
        .map_err(|e| format!("{}: {e}", into.display()))
        .and_then(|()| match file.ends_with(".zip") {
            true => unzip(archive, into, &mut written),
            false => untar(archive, into, &mut written),
        })
        .and_then(|()| {
            let path = into.join(main);
            match written.contains(&path) {
                true => Ok(path),
                false => Err(format!("the archive has no {main}")),
            }
        });
    if done.is_err() {
        for path in &written {
            let _ = std::fs::remove_file(path);
        }
    }
    done
}
