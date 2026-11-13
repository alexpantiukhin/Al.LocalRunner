mod common;

use common::*;
use lr_abstractions::Clock;
use lr_core::Timestamp;
use std::time::Duration;

const SEC: Duration = Duration::from_secs(1);

#[tokio::test]
async fn a_runner_that_never_met_the_broker_is_not_connected_since_its_start() {
    let rig = Rig::new();
    rig.net.set_reachable(BROKER, false);
    let (control, _h) = rig.start();
    rig.advance(40 * SEC).await;
    assert_eq!(control.link(), (false, Some(Timestamp(START))));
}

#[tokio::test]
async fn a_welcomed_runner_is_connected() {
    let rig = Rig::new();
    let (control, _h) = rig.start();
    let _b = Broker::accept(&rig).await;
    rig.settle().await;
    assert_eq!(control.link(), (true, None));
}

#[tokio::test]
async fn after_the_broker_goes_away_the_break_starts_at_the_loss() {
    let rig = Rig::new();
    let (control, _h) = rig.start();
    let b = Broker::accept(&rig).await;
    rig.settle().await;
    rig.advance(10 * SEC).await;
    rig.net.set_reachable(BROKER, false);
    let lost = rig.clock.now();
    b.t.close().await;
    rig.settle().await;
    assert_eq!(control.link(), (false, Some(lost)));
    rig.advance(90 * SEC).await;
    assert_eq!(control.link(), (false, Some(lost)));
}
