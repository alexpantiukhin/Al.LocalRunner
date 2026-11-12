mod common;

use common::TempDir;
use lr_cli::commands::admin_cmd::{broker_add, unreachable_message, write_code_file};
use lr_cli::commands::join_cmd::join_code;
use lr_cli::paths::Layout;
use lr_cli::roles::start_broker;
use lr_proto::AdminRequest;

fn layout(dir: &TempDir) -> Layout {
    let root = dir.path().join("state");
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let user = lr_net::current_os_user().unwrap();
    let policy =
        format!("[listen]\naddress = \"127.0.0.1:0\"\n\n[[local_user]]\nname = {user:?}\n");
    std::fs::write(config.join("policy.toml"), policy).unwrap();
    Layout::new(root, config)
}

fn add() -> AdminRequest {
    AdminRequest::BrokerAdd {
        name: Some("one".into()),
        label: None,
        runners: None,
        address: None,
    }
}

#[test]
fn a_denied_admin_channel_names_the_cause_and_the_action() {
    for denied in [
        "unreachable: Access is denied. (os error 5)",
        "unreachable: Отказано в доступе. (os error 5)",
        "unreachable: Permission denied (os error 13)",
    ] {
        let text = unreachable_message(denied);
        assert!(text.contains("administrator"), "{text}");
        assert!(text.contains("--code-file"), "{text}");
        assert!(text.contains(denied), "{text}");
    }
}

#[test]
fn an_absent_broker_is_not_blamed_on_rights() {
    let text = unreachable_message("unreachable: The system cannot find the file (os error 2)");
    assert!(!text.contains("administrator"), "{text}");
    assert!(text.contains("os error 2"), "{text}");
}

#[test]
fn the_code_file_holds_only_the_code_and_is_never_overwritten() {
    let dir = TempDir::new("codefile");
    let path = dir.path().join("code.txt");
    write_code_file(&path, "abc-123").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc-123\n");
    assert_eq!(join_code(None, Some(path.clone())).unwrap(), "abc-123");
    let again = write_code_file(&path, "other").unwrap_err();
    assert!(again.contains("already exists"), "{again}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc-123\n");
}

#[cfg(unix)]
#[test]
fn the_code_file_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("codefile-mode");
    let path = dir.path().join("code.txt");
    write_code_file(&path, "abc-123").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broker_add_with_a_code_file_writes_the_code_there() {
    let dir = TempDir::new("add-file");
    let layout = layout(&dir);
    let broker = start_broker(&layout, false).await.expect("broker starts");
    let path = dir.path().join("code.txt");
    let code = broker_add(&layout, add(), Some(path.clone()))
        .await
        .unwrap();
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    let code = text.trim();
    assert!(
        !code.is_empty() && !code.contains(char::is_whitespace),
        "{text:?}"
    );
    broker.stop();
}
