use clap::{CommandFactory, Parser};
use lr_cli::commands::Cli;
use lr_cli::config_edit::{
    channel_pipes, child_args, elevated_command_line, new_channel, rerun_elevated, sudo_route,
    Elevator, SudoRoute, ELEVATED_CHILD,
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

const ODD: &str = "a $HOME `id` \"q\" 'q' %PATH% ^& |\nnext \\\" end\\";

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
fn elevated_command_line_keeps_shell_specials_literal() {
    let given = args(&[ODD]);
    assert_eq!(
        elevated_command_line(Os::Linux, Path::new("/bin/lr"), &given),
        "sudo '/bin/lr' 'a $HOME `id` \"q\" '\\''q'\\'' %PATH% ^& |\nnext \\\" end\\'"
    );
    assert_eq!(
        elevated_command_line(Os::Windows, Path::new(r"C:\lr.exe"), &given),
        "Start-Process -Verb RunAs -Wait -FilePath 'C:\\lr.exe' -ArgumentList '\"a $HOME `id` \\\"q\\\" ''q'' %PATH% ^& |\nnext \\\\\\\" end\\\\\"'"
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
    let plain = args(&["config", "set", "a", "1"]);
    let once = child_args(&plain, None);
    assert_eq!(once, args(&[ELEVATED_CHILD, "config", "set", "a", "1"]));
    assert_eq!(child_args(&once, None), once);
    let channel = "0123456789abcdef0123456789abcdef";
    let windows = child_args(&once, Some(channel));
    assert_eq!(windows[0], format!("{ELEVATED_CHILD}={channel}"));
    assert_eq!(&windows[1..], &plain[..]);
    assert_eq!(child_args(&windows, None), once);
    assert_eq!(ELEVATED_CHILD, "--elevated-child");
}

#[test]
fn a_value_equal_to_the_marker_is_kept_as_a_value() {
    let given = args(&["config", "set", "a", ELEVATED_CHILD]);
    assert_eq!(
        child_args(&given, None),
        args(&[ELEVATED_CHILD, "config", "set", "a", ELEVATED_CHILD])
    );
    let twice = args(&[ELEVATED_CHILD, ELEVATED_CHILD, "x"]);
    assert_eq!(child_args(&twice, None), twice);
}

#[test]
fn the_cli_reads_the_marker_only_before_the_subcommand() {
    let child =
        Cli::try_parse_from(["localrunner", "--elevated-child", "runner", "config", "r1"]).unwrap();
    assert!(child.is_elevated_child());
    assert_eq!(child.elevated_child.as_deref(), Some(""));
    let channel = Cli::try_parse_from([
        "localrunner",
        "--elevated-child=0123456789abcdef0123456789abcdef",
        "status",
    ])
    .unwrap();
    assert_eq!(
        channel.elevated_child.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    let plain = Cli::try_parse_from(["localrunner", "status"]).unwrap();
    assert!(!plain.is_elevated_child());
    assert!(Cli::try_parse_from(["localrunner", "status", "--elevated-child"]).is_err());
    let value =
        Cli::try_parse_from(["localrunner", "runner", "config", "--", ELEVATED_CHILD]).unwrap();
    assert!(!value.is_elevated_child());
    let help = Cli::command().render_long_help().to_string();
    assert!(!help.contains("elevated"), "{help}");
}

#[test]
fn rerun_passes_the_arguments_and_returns_the_child_exit_code() {
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
    assert_eq!(seen[0].1, args(&["config", "set", "a", "1"]));
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

#[test]
fn a_refusal_that_names_the_command_is_not_repeated() {
    let line = elevated_command_line(Os::Linux, Path::new("/bin/lr"), &args(&["a"]));
    let fake = FakeElevator::new(Err(format!("no terminal: run it by hand: {line}")));
    let mut err = Vec::new();
    let code = rerun_elevated(
        &fake,
        Os::Linux,
        Path::new("/bin/lr"),
        &args(&["a"]),
        &mut err,
    );
    assert_eq!(code, 1);
    let text = String::from_utf8(err).unwrap();
    assert_eq!(text.matches(&line).count(), 1, "{text}");
}

#[test]
fn sudo_asks_only_with_a_terminal_or_without_a_password() {
    assert_eq!(
        sudo_route(true, || panic!("not asked with a terminal")),
        SudoRoute::Prompt
    );
    assert_eq!(sudo_route(false, || true), SudoRoute::NoPassword);
    assert_eq!(sudo_route(false, || false), SudoRoute::ByHand);
}

#[test]
fn a_channel_is_128_random_bits_and_nothing_else_is_accepted() {
    let a = new_channel().unwrap();
    let b = new_channel().unwrap();
    assert_ne!(a, b);
    let [out, err] = channel_pipes(&a).unwrap();
    assert_eq!(out, format!(r"\\.\pipe\localrunner-elevate-{a}-out"));
    assert_eq!(err, format!(r"\\.\pipe\localrunner-elevate-{a}-err"));
    for bad in [
        "",
        "0123456789ABCDEF0123456789abcdef",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        r"..\..\C:\Windows\x",
        "0123456789abcdef/123456789abcdef",
    ] {
        assert!(channel_pipes(bad).is_err(), "{bad}");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use lr_cli::config_edit::OutputChannel;
    use lr_cli::os_service::quote::windows_command_line;
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lr-cfg-elevate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_child_output_arrives_through_the_channel_and_not_its_console() {
        let dir = scratch("channel");
        let channel = new_channel().unwrap();
        let (out, err) = (Sink::default(), Sink::default());
        let pipes =
            OutputChannel::open(&channel, Box::new(out.clone()), Box::new(err.clone())).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .arg(format!("{ELEVATED_CHILD}={channel}"))
            .arg("--state-dir")
            .arg(&dir)
            .args(["profile", "check"])
            .current_dir(&dir)
            .output()
            .unwrap();
        pipes.finish();
        assert!(child.stdout.is_empty(), "{:?}", child.stdout);
        assert!(child.stderr.is_empty(), "{:?}", child.stderr);
        assert_ne!(child.status.code(), Some(0));
        assert!(err.text().contains("profiles.toml"), "{}", err.text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_channel_cannot_be_opened_twice() {
        let channel = new_channel().unwrap();
        let first = OutputChannel::open(
            &channel,
            Box::new(Sink::default()),
            Box::new(Sink::default()),
        )
        .unwrap();
        assert!(OutputChannel::open(
            &channel,
            Box::new(Sink::default()),
            Box::new(Sink::default())
        )
        .is_err());
        first.finish();
    }

    #[test]
    fn a_child_with_a_bad_channel_stops_before_the_command() {
        let child = Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .arg(format!(r"{ELEVATED_CHILD}=..\..\x"))
            .arg("status")
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(2));
        let text = String::from_utf8_lossy(&child.stderr);
        assert!(text.contains("32 lowercase hex digits"), "{text}");
    }

    #[test]
    fn the_windows_command_line_reaches_the_child_unchanged() {
        let child = Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .raw_arg(windows_command_line(&args(&[ODD])))
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(2));
        let text = String::from_utf8_lossy(&child.stderr);
        assert!(text.contains(ODD), "{text}");
    }
}
