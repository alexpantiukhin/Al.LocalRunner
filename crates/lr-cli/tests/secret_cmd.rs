mod common;

use common::fake_roles::{with_runner, FakeControl, FakeElevator};
use common::TempDir;
use lr_cli::commands::secret_cmd::{
    list_lines, remove_with, set_with, valid_name, SetArgs, ValueSource,
};
use lr_cli::paths::Layout;
use std::io::Cursor;

const RUNNER: &str = "mem_mb = 1024\ncpu = 1\n\n[env]\npath = \"/bin\"\n";

fn layout(dir: &TempDir) -> Layout {
    let layout = with_runner(dir);
    std::fs::write(layout.runner_dir("r1").join("runner.toml"), RUNNER).unwrap();
    layout
}

fn args(name: &str, source: ValueSource) -> SetArgs {
    SetArgs {
        runner: "r1".into(),
        name: name.into(),
        source,
        dry_run: false,
        restart: false,
    }
}

async fn set(layout: &Layout, a: SetArgs, input: &str) -> (Result<i32, String>, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let roles = FakeControl::new(false, false);
    let elevator = FakeElevator::new(Ok(0));
    let mut stdin = Cursor::new(input.as_bytes().to_vec());
    let code = set_with(
        layout, a, false, &elevator, &roles, &mut stdin, &mut out, &mut err,
    )
    .await;
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn a_secret_name_is_letters_digits_dash_and_underscore() {
    assert!(valid_name("CARGO_TOKEN"));
    assert!(valid_name("a-b_1"));
    assert!(!valid_name(""));
    assert!(!valid_name("a.b"));
    assert!(!valid_name("../x"));
    assert!(!valid_name("a b"));
}

#[tokio::test]
async fn set_stores_the_value_in_a_private_file_and_points_the_runner_at_it() {
    let dir = TempDir::new("secret-set");
    let layout = layout(&dir);
    let (code, out) = set(&layout, args("TOKEN", ValueSource::Stdin), "s3cret\n").await;
    assert_eq!(code, Ok(0), "{out}");
    let file = layout.root.join("secrets").join("r1").join("TOKEN");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "s3cret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let toml = std::fs::read_to_string(layout.runner_dir("r1").join("runner.toml")).unwrap();
    assert!(
        toml.contains(&format!("from = \"file:{}\"", file.display())),
        "{toml}"
    );
    assert!(!out.contains("s3cret"), "the value is never printed: {out}");
}

#[tokio::test]
async fn dry_run_writes_neither_the_file_nor_the_config() {
    let dir = TempDir::new("secret-dry");
    let layout = layout(&dir);
    let mut a = args("TOKEN", ValueSource::Stdin);
    a.dry_run = true;
    let (code, out) = set(&layout, a, "s3cret").await;
    assert_eq!(code, Ok(0), "{out}");
    assert!(!layout.root.join("secrets").exists());
    let toml = std::fs::read_to_string(layout.runner_dir("r1").join("runner.toml")).unwrap();
    assert_eq!(toml, RUNNER);
    assert!(!out.contains("s3cret"), "{out}");
}

#[tokio::test]
async fn an_empty_value_and_a_bad_name_are_refused_before_anything_is_written() {
    let dir = TempDir::new("secret-bad");
    let layout = layout(&dir);
    let (code, _) = set(&layout, args("TOKEN", ValueSource::Stdin), "\n").await;
    assert!(code.unwrap_err().contains("empty"));
    let (code, _) = set(&layout, args("a/b", ValueSource::Stdin), "x").await;
    assert!(code.unwrap_err().contains("name"));
    assert!(!layout.root.join("secrets").exists());
}

#[tokio::test]
async fn list_shows_sources_and_whether_they_give_a_value_never_the_value() {
    let dir = TempDir::new("secret-list");
    let layout = layout(&dir);
    set(&layout, args("TOKEN", ValueSource::Stdin), "s3cret")
        .await
        .0
        .unwrap();
    let mut text = std::fs::read_to_string(layout.runner_dir("r1").join("runner.toml")).unwrap();
    text.push_str("\n[secret.GONE]\nfrom = \"env:LR_SURELY_UNSET_VARIABLE\"\n");
    std::fs::write(layout.runner_dir("r1").join("runner.toml"), text).unwrap();
    let lines = list_lines(&layout, None).unwrap().join("\n");
    assert!(lines.contains("TOKEN"), "{lines}");
    assert!(lines.contains("GONE"), "{lines}");
    assert!(lines.contains("env:LR_SURELY_UNSET_VARIABLE"), "{lines}");
    assert!(
        lines.contains("variable LR_SURELY_UNSET_VARIABLE is not set"),
        "{lines}"
    );
    assert!(!lines.contains("s3cret"), "{lines}");
}

#[tokio::test]
async fn remove_takes_the_entry_out_and_deletes_its_file() {
    let dir = TempDir::new("secret-remove");
    let layout = layout(&dir);
    set(&layout, args("TOKEN", ValueSource::Stdin), "s3cret")
        .await
        .0
        .unwrap();
    let file = layout.root.join("secrets").join("r1").join("TOKEN");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = remove_with(
        &layout,
        "r1",
        "TOKEN",
        false,
        false,
        false,
        &FakeElevator::new(Ok(0)),
        &FakeControl::new(false, false),
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(code, Ok(0), "{}", String::from_utf8_lossy(&out));
    assert!(!file.exists());
    let toml = std::fs::read_to_string(layout.runner_dir("r1").join("runner.toml")).unwrap();
    assert!(!toml.contains("TOKEN"), "{toml}");
}

#[tokio::test]
async fn list_without_runners_reads_the_template_every_runner_will_copy() {
    let dir = TempDir::new("secret-template");
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let layout = Layout::new(root, config.clone());
    std::fs::write(
        config.join("runner.toml"),
        format!("{RUNNER}\n[secret.TOKEN]\nfrom = \"env:LR_SURELY_UNSET_VARIABLE\"\n"),
    )
    .unwrap();
    let lines = list_lines(&layout, None).unwrap().join("\n");
    assert!(
        lines.contains("default  TOKEN  env:LR_SURELY_UNSET_VARIABLE"),
        "{lines}"
    );
}
