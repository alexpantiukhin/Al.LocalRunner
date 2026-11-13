use clap::Parser;
use lr_cli::commands::{Cli, Command};

fn order_of(args: &[&str]) -> Vec<(String, String)> {
    let mut all = vec!["localrunner"];
    all.extend_from_slice(args);
    match Cli::try_parse_from(all).unwrap().command {
        Command::Submit { order, .. } | Command::Run { order } => order.args,
        other => panic!("{other:?}"),
    }
}

fn pair(n: &str, v: &str) -> (String, String) {
    (n.to_string(), v.to_string())
}

#[test]
fn arg_flag_repeats_and_splits_at_the_first_equals() {
    for cmd in ["submit", "run"] {
        let args = order_of(&[
            cmd, "p", "--as", "a", "--arg", "target=a", "--arg", "target=b", "--arg", "x=y=z",
        ]);
        assert_eq!(
            args,
            vec![pair("target", "a"), pair("target", "b"), pair("x", "y=z")]
        );
        assert!(order_of(&[cmd, "p", "--as", "a"]).is_empty());
    }
}

#[test]
fn malformed_arg_is_refused() {
    for bad in ["novalue", "=v", ""] {
        let err = Cli::try_parse_from(["localrunner", "run", "p", "--as", "a", "--arg", bad])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--arg"), "{err}");
    }
}

#[test]
fn empty_value_is_allowed() {
    assert_eq!(
        order_of(&["run", "p", "--as", "a", "--arg", "flag="]),
        vec![pair("flag", "")]
    );
}
