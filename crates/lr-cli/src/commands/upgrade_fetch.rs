//! `upgrade` run from the installed binary: fetch (or take from disk) a release, verify it, and
//! hand the rest to the downloaded binary, which replaces this one.

use super::args::UpgradeArgs;
use crate::paths::Layout;
use crate::release::{fetch_release, from_offline, latest_version, Fetch};
use std::path::{Path, PathBuf};

/// Runs the downloaded binary with an argument list and returns its exit code.
pub type Spawn<'a> = dyn Fn(&Path, &[String]) -> Result<i32, String> + 'a;

/// Whether `exe` is the file installed at `installed`.
pub fn is_installed(exe: &Path, installed: &Path) -> bool {
    matches!((exe.canonicalize(), installed.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// The command line the downloaded binary gets: this installation's directories, then `upgrade`
/// with the flags of this call except `--version` and `--offline`, which only this side needs.
pub fn relaunch_args(layout: &Layout, args: &UpgradeArgs) -> Vec<String> {
    let mut out = vec![
        "--state-dir".to_string(),
        layout.root.display().to_string(),
        "--config-dir".to_string(),
        layout.config.display().to_string(),
        "upgrade".to_string(),
    ];
    if let Some(dir) = &args.dir {
        out.extend(["--dir".to_string(), dir.display().to_string()]);
    }
    for (on, flag) in [
        (args.yes, "--yes"),
        (args.cancel, "--cancel"),
        (args.allow_elevated, "--allow-elevated"),
        (args.dry_run, "--dry-run"),
    ] {
        if on {
            out.push(flag.to_string());
        }
    }
    if let Some(limit) = args.wait_timeout {
        out.extend([
            "--wait-timeout".to_string(),
            format!("{}s", limit.as_secs()),
        ]);
    }
    out
}

fn scratch_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("lr-upgrade-{}-{nanos}", std::process::id()));
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder
        .create(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Gets and verifies the release the flags ask for, runs the downloaded binary through `spawn`
/// with [`relaunch_args`] and returns its exit code. Nothing is started for a release that does
/// not verify; the downloaded files are removed afterwards.
pub fn fetch_and_relaunch(
    layout: &Layout,
    args: &UpgradeArgs,
    fetch: &dyn Fetch,
    keys: &[String],
    spawn: &Spawn,
) -> Result<i32, String> {
    let dir = scratch_dir()?;
    let run = || {
        let binary = match &args.offline {
            Some(source) => {
                println!("verifying {}", source.display());
                from_offline(keys, source, &dir)?
            }
            None => {
                let version = match &args.version {
                    Some(v) => v.trim_start_matches('v').to_string(),
                    None => latest_version(fetch)?,
                };
                println!("downloading {version}");
                fetch_release(fetch, keys, &version, &dir)?
            }
        };
        println!("the signature and the SHA-256 match");
        spawn(&binary, &relaunch_args(layout, args))
    };
    let result = run();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Runs `binary` with `argv` on this terminal and returns its exit code.
pub fn spawn_inherited(binary: &Path, argv: &[String]) -> Result<i32, String> {
    std::process::Command::new(binary)
        .args(argv)
        .status()
        .map(|s| s.code().unwrap_or(1))
        .map_err(|e| format!("{}: {e}", binary.display()))
}
