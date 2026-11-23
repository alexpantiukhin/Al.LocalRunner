use lr_cli::config_edit::{
    child_args, elevated_command_line, encoded_command, rerun_elevated, windows_child_script,
    Elevator, ELEVATED_CHILD,
};
use lr_cli::os_service::Os;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

struct FakeElevator {
    reply: Result<i32, String>,
    seen: RefCell<Vec<(PathBuf, Vec<String>)>>,
}

impl FakeElevator {
    fn new(reply: Result<i32, String>) -> Self {
        FakeElevator {
            reply,
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl Elevator for FakeElevator {
    fn run_elevated(&self, exe: &Path, args: &[String]) -> Result<i32, String> {
        self.seen
            .borrow_mut()
            .push((exe.to_path_buf(), args.to_vec()));
        self.reply.clone()
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn elevated_command_line_quotes_arguments_per_os() {
    let given = args(&["config", "set", "runner.name", "it's \"x\" y"]);
    assert_eq!(
        elevated_command_line(
            Os::Windows,
            Path::new(r"C:\Program Files\lr\localrunner.exe"),
            &given
        ),
        r#"Start-Process -Verb RunAs -Wait -FilePath 'C:\Program Files\lr\localrunner.exe' -ArgumentList 'config set runner.name "it''s \"x\" y"'"#
    );
    assert_eq!(
        elevated_command_line(Os::Linux, Path::new("/opt/lr/local runner"), &given),
        r#"sudo '/opt/lr/local runner' 'config' 'set' 'runner.name' 'it'\''s "x" y'"#
    );
    assert_eq!(
        elevated_command_line(
            Os::MacOs,
            Path::new("/usr/local/bin/localrunner"),
            &args(&["a"])
        ),
        "sudo '/usr/local/bin/localrunner' 'a'"
    );
}

#[test]
fn windows_command_line_doubles_every_powershell_single_quote() {
    let given = args(&["v\u{2019}w", "tail\\"]);
    assert_eq!(
        elevated_command_line(Os::Windows, Path::new(r"C:\o'k\lr.exe"), &given),
        "Start-Process -Verb RunAs -Wait -FilePath 'C:\\o''k\\lr.exe' -ArgumentList 'v\u{2019}\u{2019}w tail\\'"
    );
}

#[test]
fn child_flag_is_appended_once() {
    let once = child_args(&args(&["config", "set", "a", "1"]));
    assert_eq!(once, args(&["config", "set", "a", "1", ELEVATED_CHILD]));
    assert_eq!(child_args(&once), once);
    assert_eq!(ELEVATED_CHILD, "--elevated-child");
}

#[test]
fn rerun_passes_the_child_flag_and_returns_the_child_exit_code() {
    let fake = FakeElevator::new(Ok(3));
    let mut err = Vec::new();
    let code = rerun_elevated(
        &fake,
        Os::Linux,
        Path::new("/bin/lr"),
        &args(&["config", "set", "a", "1"]),
        &mut err,
    );
    assert_eq!(code, 3);
    assert!(err.is_empty());
    let seen = fake.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, PathBuf::from("/bin/lr"));
    assert_eq!(
        seen[0].1,
        args(&["config", "set", "a", "1", ELEVATED_CHILD])
    );
}

#[test]
fn refusal_returns_error_and_exit_code_nonzero() {
    let fake = FakeElevator::new(Err("the administrator prompt was refused".into()));
    let mut err = Vec::new();
    let code = rerun_elevated(
        &fake,
        Os::Linux,
        Path::new("/bin/lr"),
        &args(&["config", "set", "a", "1"]),
        &mut err,
    );
    assert_ne!(code, 0);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("the administrator prompt was refused"),
        "{text}"
    );
    assert!(
        text.contains("sudo '/bin/lr' 'config' 'set' 'a' '1'"),
        "{text}"
    );
    assert!(!text.contains(ELEVATED_CHILD), "{text}");
}

#[cfg(windows)]
#[test]
fn windows_child_script_keeps_spaces_and_quotes_and_reports_the_exit_code() {
    let dir = std::env::temp_dir().join(format!("lr-cfg-elevate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let odd = "it's \"x\" y \u{2019} & %PATH% \\";
    let script = windows_child_script(
        Path::new(env!("CARGO_BIN_EXE_localrunner")),
        &args(&[odd]),
        &dir,
    );
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(encoded_command(&script))
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
    assert_eq!(read("code.txt").trim(), "2");
    let err = read("err.txt");
    assert!(err.contains(odd), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
