use clap::{CommandFactory, Parser};
use lr_cli::commands::Cli;
use lr_cli::config_edit::{
    channel_pipes, child_args, elevated_command_line, new_channel, rerun_elevated, sudo_probe_args,
    sudo_route, untrusted_text, Elevator, SudoProbe, SudoRoute, ELEVATED_CHILD,
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
fn the_cli_reads_the_expected_hash_of_the_elevated_child() {
    let child = Cli::try_parse_from([
        "localrunner",
        "--elevated-child",
        "--expect-before=ab12",
        "--state-dir",
        "s",
        "config",
        "show",
        "broker",
    ])
    .unwrap();
    assert_eq!(child.expect_before.as_deref(), Some("ab12"));
    let plain = Cli::try_parse_from(["localrunner", "status"]).unwrap();
    assert_eq!(plain.expect_before, None);
    let help = Cli::command().render_long_help().to_string();
    assert!(!help.contains("expect-before"), "{help}");
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
fn sudo_asks_only_with_a_terminal() {
    let exe = Path::new("/usr/bin/localrunner");
    assert_eq!(
        sudo_route(true, exe, &args(&["a"]), |_| panic!(
            "not probed with a terminal"
        )),
        SudoRoute::Prompt
    );
}

#[test]
fn sudo_without_a_terminal_probes_the_real_command() {
    let exe = Path::new("/usr/bin/localrunner");
    let marked = child_args(&args(&["config", "set", "a", "1"]), None);
    let mut seen = Vec::new();
    let route = sudo_route(false, exe, &marked, |argv| {
        seen = argv.to_vec();
        SudoProbe::Allowed
    });
    assert_eq!(route, SudoRoute::NoPassword);
    assert_eq!(
        seen,
        args(&[
            "-n",
            "-l",
            "/usr/bin/localrunner",
            ELEVATED_CHILD,
            "config",
            "set",
            "a",
            "1"
        ])
    );
    assert_eq!(sudo_probe_args(exe, &marked), seen);
}

fn sudoers_nopasswd_for(command: &'static str) -> impl Fn(&[String]) -> SudoProbe {
    move |argv: &[String]| {
        if argv.len() >= 3 && argv[0] == "-n" && argv[1] == "-l" && argv[2] == command {
            SudoProbe::Allowed
        } else {
            SudoProbe::NeedsPassword
        }
    }
}

#[test]
fn sudoers_that_lets_only_localrunner_run_without_a_password_is_used() {
    let probe = sudoers_nopasswd_for("/usr/bin/localrunner");
    let exe = Path::new("/usr/bin/localrunner");
    assert_eq!(
        sudo_route(false, exe, &args(&["config"]), &probe),
        SudoRoute::NoPassword
    );
}

#[test]
fn sudoers_that_lets_another_command_run_without_a_password_leaves_it_to_a_human() {
    let probe = sudoers_nopasswd_for("/usr/bin/true");
    let exe = Path::new("/usr/bin/localrunner");
    assert_eq!(
        sudo_route(false, exe, &args(&["config"]), &probe),
        SudoRoute::ByHand
    );
    assert_eq!(
        sudo_route(false, exe, &args(&["config"]), |_| SudoProbe::Missing),
        SudoRoute::Missing
    );
}

#[test]
fn a_channel_is_128_random_bits_and_this_process_and_nothing_else_is_accepted() {
    let a = new_channel().unwrap();
    let b = new_channel().unwrap();
    assert_ne!(a, b);
    let (hex, pid) = a.split_once('-').unwrap();
    assert_eq!(hex.len(), 32);
    assert_eq!(pid, std::process::id().to_string());
    let ([out, err], parent) = channel_pipes(&a).unwrap();
    assert_eq!(parent, std::process::id());
    assert_eq!(out, format!(r"\\.\pipe\localrunner-elevate-{hex}-out"));
    assert_eq!(err, format!(r"\\.\pipe\localrunner-elevate-{hex}-err"));
    for bad in [
        "",
        "0123456789abcdef0123456789abcdef",
        "0123456789ABCDEF0123456789abcdef-1",
        "0123456789abcdef0123456789abcde-1",
        "0123456789abcdef0123456789abcdef0-1",
        "0123456789abcdef0123456789abcdef-",
        "0123456789abcdef0123456789abcdef-x1",
        "0123456789abcdef0123456789abcdef-99999999999",
        "0123456789abcdef0123456789abcdef-4294967296",
        r"..\..\C:\Windows\x-1",
        "0123456789abcdef/123456789abcdef-1",
    ] {
        assert!(channel_pipes(bad).is_err(), "{bad}");
    }
}

#[test]
fn untrusted_text_drops_control_and_escape_sequences() {
    assert_eq!(
        untrusted_text(b"ok\x1b[2J\x1b]0;title\x07 line\r\n\tnext\x00\x7f\xc2\x9b"),
        "ok[2J]0;title line\n\tnext"
    );
    assert_eq!(untrusted_text("a\u{202E}b\u{2066}c".as_bytes()), "abc");
    assert_eq!(untrusted_text(b"bad \xff byte"), "bad \u{FFFD} byte");
}

#[cfg(windows)]
mod windows {
    use super::*;
    use lr_cli::config_edit::OutputChannel;
    use lr_cli::os_service::quote::windows_command_line;
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
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

    fn channel_with(out: &Sink, err: &Sink) -> (String, OutputChannel) {
        let channel = new_channel().unwrap();
        let pipes =
            OutputChannel::open(&channel, Box::new(out.clone()), Box::new(err.clone())).unwrap();
        (channel, pipes)
    }

    fn child(dir: &Path, channel: &str) -> std::process::Child {
        Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .arg(format!("{ELEVATED_CHILD}={channel}"))
            .arg("--state-dir")
            .arg(dir)
            .args(["profile", "check"])
            .current_dir(dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[test]
    fn the_child_output_arrives_through_the_channel_and_not_its_console() {
        let dir = scratch("channel");
        let (out, err) = (Sink::default(), Sink::default());
        let (channel, pipes) = channel_with(&out, &err);
        let process = child(&dir, &channel);
        let code = pipes.serve(process.as_raw_handle()).unwrap();
        let own = process.wait_with_output().unwrap();
        assert!(own.stdout.is_empty(), "{:?}", own.stdout);
        assert!(own.stderr.is_empty(), "{:?}", own.stderr);
        assert_ne!(code, 0);
        assert_eq!(Some(code), own.status.code());
        assert!(err.text().contains("profiles.toml"), "{}", err.text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_of_a_child_that_exited_before_the_parent_looked_is_kept() {
        let dir = scratch("late");
        let (out, err) = (Sink::default(), Sink::default());
        let (channel, pipes) = channel_with(&out, &err);
        let mut spawned = None;
        let code = pipes
            .serve_after_exit(|| {
                let process = child(&dir, &channel);
                let handle = process.as_raw_handle();
                spawned = Some(process);
                handle
            })
            .unwrap();
        let own = spawned.unwrap().wait_with_output().unwrap();
        assert_eq!(Some(code), own.status.code());
        assert!(own.stderr.is_empty(), "{:?}", own.stderr);
        assert!(err.text().contains("profiles.toml"), "{}", err.text());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_client_that_connects_first_is_dropped_and_the_child_is_served() {
        let dir = scratch("foreign");
        let (out, err) = (Sink::default(), Sink::default());
        let (channel, pipes) = channel_with(&out, &err);
        let ([out_name, err_name], _) = channel_pipes(&channel).unwrap();
        let mut foreign_out = std::fs::OpenOptions::new()
            .write(true)
            .open(&out_name)
            .unwrap();
        let mut foreign_err = std::fs::OpenOptions::new()
            .write(true)
            .open(&err_name)
            .unwrap();
        foreign_out.write_all(b"forged out\n").unwrap();
        foreign_err.write_all(b"forged err\n").unwrap();
        let process = child(&dir, &channel);
        let code = pipes.serve(process.as_raw_handle()).unwrap();
        let own = process.wait_with_output().unwrap();
        assert_eq!(Some(code), own.status.code());
        assert!(!out.text().contains("forged"), "{}", out.text());
        assert!(!err.text().contains("forged"), "{}", err.text());
        assert!(err.text().contains("profiles.toml"), "{}", err.text());
        drop((foreign_out, foreign_err));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_client_holding_the_pipes_does_not_hang_the_parent() {
        let (out, err) = (Sink::default(), Sink::default());
        let (channel, pipes) = channel_with(&out, &err);
        let ([out_name, err_name], _) = channel_pipes(&channel).unwrap();
        let mut foreign_out = std::fs::OpenOptions::new()
            .write(true)
            .open(&out_name)
            .unwrap();
        let foreign_err = std::fs::OpenOptions::new()
            .write(true)
            .open(&err_name)
            .unwrap();
        foreign_out.write_all(b"forged\n").unwrap();
        let process = Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .arg("--version")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let code = pipes.serve(process.as_raw_handle()).unwrap();
        assert_eq!(code, 0);
        assert!(out.text().is_empty(), "{}", out.text());
        assert!(err.text().is_empty(), "{}", err.text());
        drop((foreign_out, foreign_err));
    }

    #[test]
    fn a_child_sends_nothing_to_pipes_another_process_serves() {
        let dir = scratch("server");
        let (out, err) = (Sink::default(), Sink::default());
        let real = new_channel().unwrap();
        let pipes =
            OutputChannel::open(&real, Box::new(out.clone()), Box::new(err.clone())).unwrap();
        let (hex, _) = real.split_once('-').unwrap();
        let claimed = format!("{hex}-4");
        let process = child(&dir, &claimed);
        let code = pipes.serve(process.as_raw_handle()).unwrap();
        let own = process.wait_with_output().unwrap();
        assert_eq!(code, 2);
        assert!(out.text().is_empty(), "{}", out.text());
        assert!(err.text().is_empty(), "{}", err.text());
        let text = String::from_utf8_lossy(&own.stderr);
        assert!(text.contains("not served by process 4"), "{text}");
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
        drop(first);
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
