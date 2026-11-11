mod common;

use common::*;
use lr_abstractions::metric::{OUTBOX_OLDEST_AGE_SECONDS, OUTBOX_PENDING, OUTBOX_SAMPLE_S, ROLE};

const HUB: &[(&str, &str)] = &[(ROLE, "hub")];

#[tokio::test]
async fn outbox_is_sampled_at_start_and_every_period_with_role_only() {
    let mut rig = Rig::new();
    rig.local = true;
    rig.start();
    let meter = rig.meter.clone();
    eventually("first sample", || {
        meter.gauge(OUTBOX_PENDING, HUB) == Some(0.0)
    })
    .await;
    assert_eq!(meter.gauge(OUTBOX_OLDEST_AGE_SECONDS, HUB), Some(0.0));
    let cli = rig.cli().await;
    let job = cli.submit(submit("unit", None)).await;
    assert_eq!(rig.pending().await.len(), 1);
    rig.step(OUTBOX_SAMPLE_S - 1).await;
    assert_eq!(
        meter.gauge(OUTBOX_PENDING, HUB),
        Some(0.0),
        "not sampled yet"
    );
    rig.step(1).await;
    eventually("backlog sampled", || {
        meter.gauge(OUTBOX_PENDING, HUB) == Some(1.0)
    })
    .await;
    assert_eq!(
        meter.gauge(OUTBOX_OLDEST_AGE_SECONDS, HUB),
        Some(OUTBOX_SAMPLE_S as f64)
    );
    for (k, v) in meter.label_pairs() {
        assert_eq!((k.as_str(), v.as_str()), ("role", "hub"));
        assert!(!v.contains(&job.0));
    }
}
