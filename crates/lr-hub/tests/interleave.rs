mod common;

use common::broker::Script;
use common::*;
use std::collections::BTreeSet;

#[tokio::test]
async fn jobs_of_one_session_are_separate_orders_with_other_jobs_between_them() {
    let mut rig = Rig::new();
    let local = rig.local_broker(Script::local_runner());
    rig.start();
    let mine = rig.cli().await;
    let other = rig.cli().await;
    let mut agent = submit("unit", None);
    let mut colleague = submit("unit", None);
    colleague.identity = identity("colleague");
    let a1 = mine.submit(agent.clone()).await;
    eventually("a1 placed", || local.orders().len() == 1).await;
    let b1 = other.submit(colleague).await;
    eventually("b1 placed", || local.orders().len() == 2).await;
    agent.identity.note = Some("second".into());
    let a2 = mine.submit(agent).await;
    eventually("a2 placed", || local.orders().len() == 3).await;
    let orders = local.orders();
    let jobs: Vec<_> = orders.iter().map(|o| o.job.clone()).collect();
    assert_eq!(jobs, vec![a1.clone(), b1, a2.clone()]);
    assert_eq!(
        jobs.iter().collect::<BTreeSet<_>>().len(),
        3,
        "each job is its own order"
    );
    assert_eq!(orders[0].spec.identity.label, "agent");
    assert_eq!(orders[1].spec.identity.label, "colleague");
    assert_eq!(orders[2].spec.identity.label, "agent");
    assert_ne!(a1, a2);
}
