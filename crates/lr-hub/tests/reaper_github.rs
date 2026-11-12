mod common;

use common::broker::Script;
use common::*;
use lr_abstractions::ProcessRef;
use lr_core::{GithubInfo, Source};
use lr_proto::{ReasonCode, SubmitRequest};

const RUN_PID: u32 = 7777;
const RUN_START: u64 = 4242;

fn from_a_workflow(detach: bool) -> SubmitRequest {
    let mut req = submit("unit", None);
    req.identity.label = "ci".into();
    req.identity.sender_pid = RUN_PID;
    req.identity.sender_start = Some(RUN_START);
    req.identity.source = Source::Github(GithubInfo {
        repository: "acme/app".into(),
        workflow: "checks".into(),
        job: "unit".into(),
        run_id: 99,
        attempt: Some(1),
        git_ref: "refs/heads/main".into(),
        actor: "alex".into(),
        run_url: "https://github.com/acme/app/actions/runs/99".into(),
    });
    req.detach = detach;
    req
}

#[tokio::test]
async fn a_cancelled_workflow_kills_its_run_and_the_reaper_cancels_the_order() {
    let mut rig = Rig::new();
    rig.inspector.add(ProcessRef {
        pid: RUN_PID,
        start: Some(RUN_START),
    });
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let cli = rig.cli().await;
    let job = cli.submit(from_a_workflow(false)).await;
    let detached = cli.submit(from_a_workflow(true)).await;
    eventually("accepted", || local.orders().len() == 2).await;
    settle().await;
    local.started(&job).await;
    settle().await;

    rig.inspector.remove(RUN_PID);
    rig.step(7).await;
    eventually("cancelled", || !local.cancels().is_empty()).await;
    rig.step(10).await;
    let cancels = local.cancels();
    assert_eq!(cancels.len(), 1, "the detached order stays");
    assert_eq!(cancels[0].job, job);
    assert_eq!(cancels[0].code, ReasonCode::SenderGone);
    assert!(rig.active(&detached).await.is_some());
}
