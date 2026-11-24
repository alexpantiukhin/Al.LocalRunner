mod common;

use common::fake_roles::{with_runner, FakeControl, FakeElevator};
use common::TempDir;
use lr_broker::policy::{check, load, HostResources, PolicyFile};
use lr_cli::commands::down_cmd::Target as Role;
use lr_cli::commands::policy_edit_cmd::{edit_with, Record, RecordArgs};
use lr_cli::config_edit::{
    add_local_user, add_name, add_shared, remove_local_user, remove_name, remove_shared, ConfigDoc,
};
use lr_cli::paths::Layout;
use std::path::PathBuf;

struct Run {
    code: Result<i32, String>,
    out: String,
    err: String,
}

fn policy(layout: &Layout) -> PathBuf {
    layout.config_file("policy.toml")
}

fn layout_with(dir: &TempDir, text: &str) -> Layout {
    let layout = with_runner(dir);
    std::fs::write(policy(&layout), text).unwrap();
    layout
}

fn args(record: Record) -> RecordArgs {
    RecordArgs {
        record,
        dry_run: false,
        restart: false,
        expected_before: None,
    }
}

async fn run_with(
    layout: &Layout,
    args: RecordArgs,
    roles: &FakeControl,
    elevator: &FakeElevator,
) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = edit_with(layout, args, false, elevator, roles, &mut out, &mut err).await;
    Run {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

async fn run(layout: &Layout, record: Record, roles: &FakeControl) -> Run {
    run_with(layout, args(record), roles, &FakeElevator::new(Ok(0))).await
}

fn edited<T>(text: &str, edit: impl FnOnce(&mut ConfigDoc) -> Result<T, String>) -> (T, String) {
    let mut doc = ConfigDoc::parse(text).unwrap();
    let changed = edit(&mut doc).unwrap();
    (changed, doc.render())
}

fn loaded(text: &str) -> PolicyFile {
    load(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn secret(name: &str, label: &str) -> Record {
    Record::AllowSecret {
        name: name.into(),
        label: label.into(),
    }
}

fn service(name: &str, label: &str) -> Record {
    Record::AllowService {
        name: name.into(),
        label: label.into(),
    }
}

const TWO_SECRETS: &str = "# policy\n[[secrets.allow]]\n# ci entry\nlabel = \"ci\"\nnames = [\"a\"]\n\n[[secrets.allow]]\nlabel = \"dev\"\nnames = [\"b\"]\n";

#[test]
fn allow_secret_appends_name_to_label_entry() {
    let (changed, text) = edited(TWO_SECRETS, |d| add_name(d, "secrets.allow", "dev", "c"));
    assert!(changed);
    assert_eq!(text, TWO_SECRETS.replace("[\"b\"]", "[\"b\", \"c\"]"));
}

#[test]
fn allow_secret_creates_entry() {
    let start = "# my policy\n[queue]\nstrategy = \"fair-share\"\n";
    let (changed, text) = edited(start, |d| add_name(d, "secrets.allow", "ci", "token"));
    assert!(changed);
    assert!(text.starts_with(start), "{text}");
    let file = loaded(&text);
    assert_eq!(file.secrets_allowed("ci"), ["token".to_string()]);
    let (changed, text) = edited(TWO_SECRETS, |d| add_name(d, "secrets.allow", "ops", "x"));
    assert!(changed);
    assert!(text.starts_with(TWO_SECRETS), "{text}");
    assert_eq!(loaded(&text).secrets_allowed("ops"), ["x".to_string()]);
}

#[test]
fn an_inline_list_stays_inline() {
    let start = "[secrets]\nallow = [{ label = \"ci\", names = [\"a\"] }] # keep\n\n[queue]\nstrategy = \"fair-share\"\n";
    let (changed, text) = edited(start, |d| add_name(d, "secrets.allow", "ci", "a"));
    assert!(!changed);
    assert_eq!(text, start);
    let (changed, text) = edited(start, |d| add_name(d, "secrets.allow", "ci", "b"));
    assert!(changed);
    assert_eq!(text, start.replace("[\"a\"]", "[\"a\", \"b\"]"));
    let (changed, two) = edited(start, |d| add_name(d, "secrets.allow", "dev", "x"));
    assert!(changed);
    let both = "[{ label = \"ci\", names = [\"a\"] }, { label = \"dev\", names = [\"x\"] }]";
    assert_eq!(
        two,
        start.replace("[{ label = \"ci\", names = [\"a\"] }]", both)
    );
    assert_eq!(loaded(&two).secrets_allowed("dev"), ["x".to_string()]);
    let (removed, text) = edited(&two, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 1);
    assert_eq!(
        text,
        start.replace(
            "{ label = \"ci\", names = [\"a\"] }",
            "{ label = \"dev\", names = [\"x\"] }"
        )
    );
    let (removed, text) = edited(&two, |d| remove_name(d, "secrets.allow", "dev", "x"));
    assert_eq!(removed, 1);
    assert_eq!(text, start);
}

#[test]
fn an_inline_parent_table_stays_inline() {
    let start = "secrets = { allow = [{ label = \"ci\", names = [\"a\"] }] }\n";
    let (changed, text) = edited(start, |d| add_name(d, "secrets.allow", "ci", "b"));
    assert!(changed);
    assert_eq!(text, start.replace("[\"a\"]", "[\"a\", \"b\"]"));
    let (changed, text) = edited("services = { evict_idle = true }\n", |d| {
        add_name(d, "services.allow", "ci", "db")
    });
    assert!(changed);
    assert!(text.starts_with("services = { evict_idle = true"), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(loaded(&text).service_allowed("ci", "db"), "{text}");
}

#[test]
fn remove_takes_every_duplicate_of_the_name() {
    let start = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\", \"a\", \"b\"]\n";
    let (removed, text) = edited(start, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 2);
    assert_eq!(loaded(&text).secrets_allowed("ci"), ["b".to_string()]);
    assert_eq!(text, start.replace("[\"a\", \"a\", \"b\"]", "[\"b\"]"));

    let only = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\", \"a\"]\n\n[[secrets.allow]]\nlabel = \"dev\"\nnames = [\"b\"]\n";
    let (removed, text) = edited(only, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 2);
    let file = loaded(&text);
    assert_eq!(file.secrets.allow.len(), 1);
    assert!(file.secrets_allowed("ci").is_empty());

    let later = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\", \"a\"]\n\n[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"b\"]\n";
    let (removed, text) = edited(later, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 2);
    let file = loaded(&text);
    assert_eq!(file.secrets.allow.len(), 2);
    assert!(file.secrets_allowed("ci").is_empty(), "{text}");

    let services = "[[services.allow]]\nlabel = \"ci\"\nnames = [\"db\", \"db\"]\n";
    let (removed, text) = edited(services, |d| remove_name(d, "services.allow", "ci", "db"));
    assert_eq!(removed, 2);
    let file = loaded(&text);
    assert_eq!(file.services.allow.len(), 1, "{text}");
    assert!(!file.service_allowed("ci", "db"));
    assert!(!file.service_allowed("dev", "db"));
}

#[tokio::test]
async fn remove_says_how_many_were_removed() {
    let dir = TempDir::new("records-remove-count");
    let layout = layout_with(
        &dir,
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\", \"a\", \"b\"]\n",
    );
    let roles = FakeControl::new(false, true);
    let r = run(
        &layout,
        Record::RemoveSecret {
            name: "a".into(),
            label: "ci".into(),
        },
        &roles,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("removed 2 occurrences"), "{}", r.out);
    let after = std::fs::read_to_string(policy(&layout)).unwrap();
    assert_eq!(loaded(&after).secrets_allowed("ci"), ["b".to_string()]);
}

const MULTILINE: &str = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\n    # first\n    \"a\",\n    # second\n    \"b\",\n]\n";

#[test]
fn removing_a_name_keeps_the_comments_of_the_others() {
    let (removed, text) = edited(MULTILINE, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 1);
    assert_eq!(
        text,
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\n    # second\n    \"b\",\n]\n"
    );
    let (removed, text) = edited(MULTILINE, |d| remove_name(d, "secrets.allow", "ci", "b"));
    assert_eq!(removed, 1);
    assert_eq!(
        text,
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\n    # first\n    \"a\",\n]\n"
    );
}

#[test]
fn removing_a_table_keeps_the_comment_above_the_next_header() {
    let start = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\"]\n\n# dev entry\n[[secrets.allow]]\nlabel = \"dev\"\nnames = [\"b\"]\n\n# queue settings\n[queue]\nstrategy = \"fair-share\"\n";
    let (removed, text) = edited(start, |d| remove_name(d, "secrets.allow", "dev", "b"));
    assert_eq!(removed, 1);
    assert_eq!(
        text,
        "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\"]\n\n# queue settings\n[queue]\nstrategy = \"fair-share\"\n"
    );
    let (removed, text) = edited(start, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(removed, 1);
    assert_eq!(
        text,
        "\n# dev entry\n[[secrets.allow]]\nlabel = \"dev\"\nnames = [\"b\"]\n\n# queue settings\n[queue]\nstrategy = \"fair-share\"\n"
    );
}

#[tokio::test]
async fn allow_secret_twice_is_noop() {
    let dir = TempDir::new("records-secret-twice");
    let layout = layout_with(&dir, TWO_SECRETS);
    let roles = FakeControl::new(false, true);
    let r = run(&layout, secret("c", "dev"), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("+names = [\"b\", \"c\"]"), "{}", r.out);
    let after = std::fs::read_to_string(policy(&layout)).unwrap();
    let r = run(&layout, secret("c", "dev"), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("already present"), "{}", r.out);
    assert!(r.out.contains("no change in"), "{}", r.out);
    assert_eq!(std::fs::read_to_string(policy(&layout)).unwrap(), after);
    assert_eq!(roles.reloads(), [Role::Broker, Role::Broker]);
}

#[test]
fn remove_secret_drops_empty_entry() {
    let (changed, text) = edited(TWO_SECRETS, |d| remove_name(d, "secrets.allow", "dev", "b"));
    assert_eq!(changed, 1);
    let file = loaded(&text);
    assert_eq!(file.secrets.allow.len(), 1);
    assert!(file.secrets_allowed("dev").is_empty());
    assert!(text.contains("# ci entry"), "{text}");
    let (_, two) = edited(TWO_SECRETS, |d| add_name(d, "secrets.allow", "dev", "c"));
    let (changed, text) = edited(&two, |d| remove_name(d, "secrets.allow", "dev", "b"));
    assert_eq!(changed, 1);
    assert!(text.contains("names = [\"c\"]"), "{text}");
}

#[test]
fn remove_secret_keeps_an_empty_entry_when_a_later_entry_of_the_label_would_decide() {
    let start = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\"]\n\n[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"b\"]\n";
    let (changed, text) = edited(start, |d| remove_name(d, "secrets.allow", "ci", "a"));
    assert_eq!(changed, 1);
    let file = loaded(&text);
    assert_eq!(file.secrets.allow.len(), 2);
    assert!(file.secrets_allowed("ci").is_empty(), "{text}");
}

#[tokio::test]
async fn remove_missing_is_error() {
    let dir = TempDir::new("records-remove-missing");
    let layout = layout_with(&dir, TWO_SECRETS);
    let roles = FakeControl::new(false, true);
    for record in [
        Record::RemoveSecret {
            name: "zzz".into(),
            label: "ci".into(),
        },
        Record::RemoveSecret {
            name: "a".into(),
            label: "nobody".into(),
        },
        Record::RemoveService {
            name: "db".into(),
            label: "ci".into(),
        },
        Record::RemoveLocalUser {
            name: "nobody".into(),
        },
        Record::RemoveShared {
            members: vec!["x".into(), "y".into()],
        },
    ] {
        let r = run(&layout, record.clone(), &roles).await;
        let e = r.code.as_ref().unwrap_err();
        assert!(e.contains("not in"), "{record:?}: {e}");
        assert!(e.contains(&policy(&layout).display().to_string()), "{e}");
    }
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        TWO_SECRETS
    );
    assert!(roles.reloads().is_empty());
}

#[tokio::test]
async fn empty_label_warns() {
    let dir = TempDir::new("records-empty-label");
    let layout = layout_with(&dir, "");
    let roles = FakeControl::new(false, true);
    let r = run(&layout, secret("token", ""), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    let after = std::fs::read_to_string(policy(&layout)).unwrap();
    let host = HostResources {
        mem_mb: 8192,
        cpu: 4,
    };
    let reload_warning = check(&loaded(&after), &host)
        .iter()
        .map(ToString::to_string)
        .find(|w| w.contains("label ``"))
        .expect("policy reload warns about the empty label");
    assert!(
        r.out.contains(&format!("warning: {reload_warning}")),
        "{}",
        r.out
    );
    assert_eq!(loaded(&after).secrets_allowed(""), ["token".to_string()]);
}

#[test]
fn allow_service_appends_name_to_label_entry() {
    let start = TWO_SECRETS.replace("secrets", "services");
    let (changed, text) = edited(&start, |d| add_name(d, "services.allow", "dev", "c"));
    assert!(changed);
    assert_eq!(text, start.replace("[\"b\"]", "[\"b\", \"c\"]"));
}

#[test]
fn allow_service_creates_entry() {
    let (changed, text) = edited("", |d| add_name(d, "services.allow", "ci", "db"));
    assert!(changed);
    let file = loaded(&text);
    assert!(file.service_allowed("ci", "db"));
    assert!(!file.service_allowed("dev", "db"));
}

#[tokio::test]
async fn allow_service_twice_is_noop() {
    let dir = TempDir::new("records-service-twice");
    let layout = layout_with(&dir, "");
    let roles = FakeControl::new(false, true);
    let r = run(&layout, service("db", "ci"), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    let after = std::fs::read_to_string(policy(&layout)).unwrap();
    let r = run(&layout, service("db", "ci"), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("already present"), "{}", r.out);
    assert_eq!(std::fs::read_to_string(policy(&layout)).unwrap(), after);
    assert_eq!(roles.reloads(), [Role::Broker, Role::Broker]);
}

#[test]
fn remove_service_drops_empty_entry() {
    let start = TWO_SECRETS.replace("secrets", "services");
    let (changed, text) = edited(&start, |d| remove_name(d, "services.allow", "dev", "b"));
    assert_eq!(changed, 1);
    let file = loaded(&text);
    assert_eq!(file.services.allow.len(), 1);
    assert!(!file.service_allowed("dev", "b"));
}

#[test]
fn remove_last_service_name_keeps_the_list_closed() {
    let (_, start) = edited("", |d| add_name(d, "services.allow", "ci", "db"));
    let (changed, text) = edited(&start, |d| remove_name(d, "services.allow", "ci", "db"));
    assert_eq!(changed, 1);
    let file = loaded(&text);
    assert_eq!(file.services.allow.len(), 1, "{text}");
    assert!(!file.service_allowed("ci", "db"));
    assert!(!file.service_allowed("dev", "db"));
}

#[tokio::test]
async fn allow_service_with_empty_label_is_accepted() {
    let dir = TempDir::new("records-service-empty-label");
    let layout = layout_with(&dir, "");
    let roles = FakeControl::new(false, true);
    let r = run(&layout, service("db", ""), &roles).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    let after = std::fs::read_to_string(policy(&layout)).unwrap();
    assert!(loaded(&after).service_allowed("", "db"));
}

const USERS: &str =
    "# users of this machine\n[[local_user]]\n# me\nname = \"alex\"\nlabel = \"dev\"\n";

#[test]
fn add_local_user_keeps_comments() {
    let (changed, text) = edited(USERS, |d| add_local_user(d, "bob", Some("ci")));
    assert!(changed);
    assert!(text.starts_with(USERS), "{text}");
    let file = loaded(&text);
    assert_eq!(file.local_users.len(), 2);
    assert_eq!(file.local_users[1].name, "bob");
    assert_eq!(file.local_users[1].label, "ci");
    let (changed, back) = edited(&text, |d| remove_local_user(d, "bob"));
    assert!(changed);
    assert_eq!(back, USERS);
}

#[test]
fn add_local_user_is_identified_by_name() {
    let (changed, text) = edited(USERS, |d| add_local_user(d, "alex", Some("dev")));
    assert!(!changed);
    assert_eq!(text, USERS);
    let mut doc = ConfigDoc::parse(USERS).unwrap();
    let e = add_local_user(&mut doc, "alex", Some("ci")).unwrap_err();
    assert!(e.contains("`dev`"), "{e}");
    let (changed, text) = edited(USERS, |d| add_local_user(d, "carol", None));
    assert!(changed);
    let file = loaded(&text);
    assert_eq!(file.local_users[1].label, "");
    assert!(!text.contains("name = \"carol\"\nlabel"), "{text}");
}

#[test]
fn add_shared_identifies_by_member_set() {
    let (changed, text) = edited("", |d| add_shared(d, &["a".into(), "b".into(), "a".into()]));
    assert!(changed);
    assert_eq!(loaded(&text).shared[0].members, ["a", "b"]);
    let (changed, same) = edited(&text, |d| add_shared(d, &["b".into(), "a".into()]));
    assert!(!changed);
    assert_eq!(same, text);
    let (changed, other) = edited(&text, |d| add_shared(d, &["a".into(), "c".into()]));
    assert!(changed);
    assert_eq!(loaded(&other).shared.len(), 2);
    let (changed, removed) = edited(&other, |d| remove_shared(d, &["b".into(), "a".into()]));
    assert!(changed);
    let file = loaded(&removed);
    assert_eq!(file.shared.len(), 1);
    assert_eq!(file.shared[0].members, ["a", "c"]);
    let (changed, _) = edited(&removed, |d| remove_shared(d, &["a".into(), "b".into()]));
    assert!(!changed);
}

#[tokio::test]
async fn add_local_user_and_shared_run_through_the_change_flow() {
    let dir = TempDir::new("records-flow");
    let layout = layout_with(&dir, USERS);
    let roles = FakeControl::new(false, true);
    let r = run(
        &layout,
        Record::AddLocalUser {
            name: "bob".into(),
            label: Some("ci".into()),
        },
        &roles,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("+name = \"bob\""), "{}", r.out);
    let r = run(
        &layout,
        Record::AddShared {
            members: vec!["ci".into(), "r1".into()],
        },
        &roles,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(!r.out.contains("applies at the next start"), "{}", r.out);
    let r = run(
        &layout,
        Record::AddShared {
            members: vec!["r1".into(), "ci".into()],
        },
        &roles,
    )
    .await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("already present"), "{}", r.out);
    let file = loaded(&std::fs::read_to_string(policy(&layout)).unwrap());
    assert_eq!(file.local_users.len(), 2);
    assert_eq!(file.shared.len(), 1);
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let dir = TempDir::new("records-dry-run");
    let layout = layout_with(&dir, TWO_SECRETS);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut a = args(secret("c", "dev"));
    a.dry_run = true;
    let r = run_with(&layout, a, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("dry run"), "{}", r.out);
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        TWO_SECRETS
    );
    assert!(elevator.calls().is_empty());
    assert!(roles.reloads().is_empty());
}

#[tokio::test]
async fn elevated_rerun_repeats_the_verb() {
    let dir = TempDir::new("records-elevated");
    let layout = layout_with(&dir, TWO_SECRETS);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut a = args(secret("c", ""));
    a.restart = true;
    let r = run_with(&layout, a, &roles, &elevator).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    let calls = elevator.calls();
    assert_eq!(calls.len(), 1);
    let words: Vec<String> = ["policy", "allow-secret", "c", "--label", "", "--restart"]
        .iter()
        .map(|w| w.to_string())
        .collect();
    assert!(
        calls[0]
            .1
            .windows(words.len())
            .any(|w| w == words.as_slice()),
        "{:?}",
        calls[0].1
    );
    assert_eq!(
        calls[0].1[0],
        format!(
            "--expect-before={}",
            lr_cli::runner_reload::hash_text(TWO_SECRETS)
        )
    );
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        TWO_SECRETS
    );
}

#[tokio::test]
async fn the_elevated_child_of_a_verb_refuses_a_policy_that_changed() {
    let dir = TempDir::new("records-child-changed");
    let layout = layout_with(&dir, TWO_SECRETS);
    let roles = FakeControl::new(true, true);
    let elevator = FakeElevator::new(Ok(0));
    let mut a = args(secret("c", "ci"));
    a.expected_before = Some(lr_cli::runner_reload::hash_text("[queue]\n"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = edit_with(&layout, a, true, &elevator, &roles, &mut out, &mut err).await;
    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, Ok(lr_cli::config_edit::EXIT_FILE_CHANGED), "{err}");
    assert!(err.contains("changed after its diff was shown"), "{err}");
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        TWO_SECRETS
    );
    assert!(roles.reloads().is_empty());
}

#[test]
fn policy_verbs_parse_on_the_command_line() {
    let dir = TempDir::new("records-cli");
    let layout = layout_with(&dir, TWO_SECRETS);
    let cli = |words: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_localrunner"))
            .arg("--state-dir")
            .arg(&layout.root)
            .arg("--config-dir")
            .arg(&layout.config)
            .arg("policy")
            .args(words)
            .output()
            .unwrap()
    };
    let out = cli(&["allow-secret", "c", "--label", "dev", "--dry-run"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("+names = [\"b\", \"c\"]"), "{text}");
    for words in [
        &["remove-secret", "zzz", "--label", "dev"][..],
        &["remove-service", "zzz", "--label", "dev"],
        &["remove-local-user", "nobody"],
        &["remove-shared", "x", "y"],
    ] {
        let out = cli(words);
        assert_eq!(out.status.code(), Some(1), "{words:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not in"),
            "{words:?}"
        );
    }
    let out = cli(&["add-local-user", "bob", "--label", "ci", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    let out = cli(&["add-shared", "a", "b", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    let out = cli(&["allow-service", "db", "--label", "ci", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        TWO_SECRETS
    );
}

#[tokio::test]
async fn remove_reports_nothing_removed_when_the_apply_fails() {
    let start = "[[secrets.allow]]\nlabel = \"ci\"\nnames = [\"a\", \"a\", \"b\"]\n";
    let dir = TempDir::new("records-remove-refused");
    let layout = layout_with(&dir, start);
    let remove = || {
        args(Record::RemoveSecret {
            name: "a".into(),
            label: "ci".into(),
        })
    };
    let roles = FakeControl::new(true, true);
    let refused = FakeElevator::new(Err("the elevation was refused".into()));
    let r = run_with(&layout, remove(), &roles, &refused).await;
    assert_ne!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(!r.out.contains("removed"), "{}", r.out);
    assert_eq!(std::fs::read_to_string(policy(&layout)).unwrap(), start);

    let failed_child = FakeElevator::new(Ok(1));
    let r = run_with(&layout, remove(), &roles, &failed_child).await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert!(!r.out.contains("removed"), "{}", r.out);

    let mut failing = FakeControl::new(false, true);
    failing.reload_error = Some("the broker did not answer".into());
    let r = run_with(&layout, remove(), &failing, &FakeElevator::new(Ok(0))).await;
    assert_eq!(r.code, Ok(1), "{} {}", r.out, r.err);
    assert_eq!(
        std::fs::read_to_string(policy(&layout)).unwrap(),
        start.replace("[\"a\", \"a\", \"b\"]", "[\"b\"]")
    );
    assert!(!r.out.contains("removed"), "{}", r.out);
    std::fs::write(policy(&layout), start).unwrap();

    let mut dry = remove();
    dry.dry_run = true;
    let r = run_with(&layout, dry, &roles, &FakeElevator::new(Ok(0))).await;
    assert_eq!(r.code, Ok(0), "{} {}", r.out, r.err);
    assert!(r.out.contains("would remove 2 occurrences"), "{}", r.out);
    assert!(!r.out.contains(": removed"), "{}", r.out);
    assert_eq!(std::fs::read_to_string(policy(&layout)).unwrap(), start);
}
