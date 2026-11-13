use std::io;
use std::path::{Path, PathBuf};

/// Removes `path`. A running executable cannot be deleted on Windows: it is renamed to
/// `<name>.old` and a detached helper deletes it, and the directory if it is then empty, after
/// this process has exited.
pub(super) fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e)
            if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied && is_running(path) =>
        {
            defer(path)
        }
        other => other,
    }
}

fn is_running(path: &Path) -> bool {
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    std::env::current_exe().is_ok_and(|me| same(&me, path))
}

fn defer(path: &Path) -> io::Result<()> {
    let old = old_name(path);
    std::fs::rename(path, &old)?;
    spawn_cleanup(&old)
}

pub(super) fn old_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

#[cfg(any(windows, test))]
pub(super) fn cleanup_command(old: &Path) -> String {
    let dir = old.parent().unwrap_or_else(|| Path::new("."));
    format!(
        "ping -n 4 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
        old.display(),
        dir.display()
    )
}

#[cfg(windows)]
fn spawn_cleanup(old: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .raw_arg(format!("/D /C \"{}\"", cleanup_command(old)))
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
fn spawn_cleanup(_old: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_renamed_binary_and_its_empty_directory_are_removed_after_exit() {
        let old = old_name(Path::new("C:/Programs/Al.LocalRunner/localrunner.exe"));
        assert_eq!(
            old,
            Path::new("C:/Programs/Al.LocalRunner/localrunner.exe.old")
        );
        let cmd = cleanup_command(&old);
        assert!(cmd.contains("del /f /q"), "{cmd}");
        assert!(cmd.contains("localrunner.exe.old"), "{cmd}");
        assert!(cmd.ends_with("Al.LocalRunner\""), "{cmd}");
    }

    #[test]
    fn a_file_that_is_not_running_is_removed_directly() {
        let dir = std::env::temp_dir().join(format!("lr-self-remove-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        remove_file(&file).unwrap();
        assert!(!file.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_running_executable_is_renamed_and_the_helper_deletes_it_with_its_directory() {
        const CHILD: &str = "LOCALRUNNER_SELF_REMOVE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            remove_file(&std::env::current_exe().unwrap()).unwrap();
            return;
        }
        let dir = std::env::temp_dir().join(format!("lr-self-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("child.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let status = std::process::Command::new(&exe)
            .args([
                "--exact",
                "os_service::self_remove::tests::a_running_executable_is_renamed_and_the_helper_deletes_it_with_its_directory",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!exe.exists());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while dir.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(!dir.exists(), "the helper left {}", dir.display());
    }
}
