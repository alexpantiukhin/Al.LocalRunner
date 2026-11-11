mod common;

use common::harness::*;
use lr_abstractions::fakes::{MemoryMeter, MeterUpdate};
use lr_abstractions::metric::{self, *};
use lr_core::JobState;
use lr_proto::*;
use std::time::Duration;

const LINUX: &[(&str, &str)] = &[(PLATFORM, "linux")];

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{what} never happened");
}

fn assert_clean(meter: &MemoryMeter, forbidden: &[&str]) {
    for (k, v) in meter.label_pairs() {
        for f in forbidden {
            assert!(!v.contains(f), "label {k}={v} carries {f}");
        }
        assert!(
            ["platform", "pool", "runner", "reason", "result", "role", "client"]
                .contains(&k.as_str()),
            "unexpected label {k}"
        );
    }
}

async fn tick_by(h: &Harness, secs: u64) {
    for _ in 0..secs {
        h.clock.advance(Duration::from_secs(1));
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn queue_wait_duration_and_results_by_platform() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 600))).await;
    hub.accepted().await;
    let m = &h.meter;
    assert_eq!(m.gauge(QUEUE_LENGTH, LINUX), Some(1.0));
    assert_eq!(m.observed(QUEUE_WAIT_SECONDS, LINUX), vec![0.0]);
    r.send(&started("j1", 1)).await;
    loop {
        if let BrokerToHub::Started(_) = hub.hub().await {
            break;
        }
    }
    h.clock.advance(Duration::from_secs(7));
    r.send(&result("j1", 2, JobState::Failed, 3)).await;
    r.assign().await;
    let failed = [(RESULT, "failed"), (PLATFORM, "linux")];
    assert_eq!(m.counter(ORDERS_TOTAL, &failed), 1);
    assert_eq!(m.observed(JOB_DURATION_SECONDS, &failed), vec![7.0]);
    assert_eq!(m.observed(QUEUE_WAIT_SECONDS, LINUX), vec![0.0, 7.0]);
    assert_eq!(m.gauge(QUEUE_LENGTH, LINUX), Some(0.0));
    assert_clean(
        m,
        &["j1", "j2", "alex", "owner/repo", "agent-1", "make", "/work"],
    );
}

#[tokio::test]
async fn refused_orders_count_as_rejected_without_client_text_as_platform() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(20000, 1, 60))).await;
    hub.rejected().await;
    let mut odd = req(512, 1, 60);
    odd.platform = Some("plan9-secret-name".into());
    hub.send(&order("j2", odd)).await;
    hub.rejected().await;
    let m = &h.meter;
    let rejected = |p| [(RESULT, "rejected"), (PLATFORM, p)];
    assert_eq!(m.counter(ORDERS_TOTAL, &rejected("linux")), 1);
    assert_eq!(m.counter(ORDERS_TOTAL, &rejected(OTHER_PLATFORM)), 1);
    assert!(m
        .observed(JOB_DURATION_SECONDS, &rejected("linux"))
        .is_empty());
    assert_clean(m, &["plan9", "secret", "j1", "j2", "alex"]);
}

#[tokio::test]
async fn client_label_is_offered_as_the_label_never_the_key() {
    let h = Harness::start("");
    let _r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.member_hub("laptop", "ci", "10.0.0.9").await;
    hub.send(&order("j1", req(20000, 1, 60))).await;
    hub.rejected().await;
    let labels = [(RESULT, "rejected"), (PLATFORM, "linux"), (CLIENT, "ci")];
    assert_eq!(h.meter.counter(ORDERS_TOTAL, &labels), 1);
    assert_clean(&h.meter, &["laptop", "mid-", "pk-"]);
}

#[tokio::test]
async fn pool_load_and_presence_per_runner_then_forgotten_on_removal() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let m = h.meter.clone();
    let runner = [(RUNNER, "r1")];
    assert_eq!(m.gauge(RUNNER_ONLINE, &runner), Some(1.0));
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(1536, 3, 600))).await;
    hub.accepted().await;
    let load = [(metric::POOL, POOL_VALUE), (RUNNER, "r1")];
    eventually("load reported", || {
        m.gauge(RUNNING_JOBS, &load) == Some(1.0)
    })
    .await;
    assert_eq!(m.gauge(POOL_MEMORY_USED_MB, &load), Some(1536.0));
    assert_eq!(m.gauge(POOL_CPU_USED, &load), Some(3.0));
    r.close().await;
    eventually("runner offline", || {
        m.gauge(RUNNER_ONLINE, &runner) == Some(0.0)
    })
    .await;
    assert_eq!(m.gauge(RUNNING_JOBS, &load), Some(0.0));
    h.handle.revoked("r1").await;
    assert!(m
        .updates()
        .contains(&MeterUpdate::Forget("runner".into(), "r1".into())));
    assert_eq!(m.gauge(RUNNER_ONLINE, &runner), None);
    assert_eq!(m.gauge(RUNNING_JOBS, &load), None);
}

#[tokio::test]
async fn saturation_wait_grows_for_the_waiting_reason_only() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 600))).await;
    hub.accepted().await;
    tick_by(&h, 20).await;
    let m = h.meter.clone();
    let busy = [(REASON, "runner_busy")];
    eventually("saturation reported", || {
        m.gauge(SATURATION_WAIT_SECONDS, &busy) == Some(20.0)
    })
    .await;
    for other in ["memory", "cpu", "tag", "rule"] {
        assert_eq!(
            m.gauge(SATURATION_WAIT_SECONDS, &[(REASON, other)]),
            Some(0.0),
            "{other}"
        );
    }
}

#[tokio::test]
async fn policy_reloads_count_by_result() {
    let h = Harness::start("");
    h.handle.reload_policy(&policy("")).await.unwrap();
    h.handle
        .reload_policy("[grace]\nlost_min = 0\n")
        .await
        .unwrap_err();
    h.handle
        .reload_policy("[grace]\nlost_min = 0\n")
        .await
        .unwrap_err();
    let m = &h.meter;
    assert_eq!(m.counter(POLICY_RELOAD_TOTAL, &[(RESULT, "ok")]), 1);
    assert_eq!(m.counter(POLICY_RELOAD_TOTAL, &[(RESULT, "error")]), 2);
}

#[tokio::test]
async fn result_outbox_is_sampled_with_its_oldest_age() {
    let h = Harness::start("");
    let m = h.meter.clone();
    let role = [(ROLE, "broker")];
    eventually("first sample", || {
        m.gauge(OUTBOX_PENDING, &role) == Some(0.0)
    })
    .await;
    assert_eq!(m.gauge(OUTBOX_OLDEST_AGE_SECONDS, &role), Some(0.0));
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    loop {
        if let BrokerToHub::Started(_) = hub.hub().await {
            break;
        }
    }
    tick_by(&h, OUTBOX_SAMPLE_S).await;
    eventually("backlog sampled", || {
        m.gauge(OUTBOX_PENDING, &role) == Some(1.0)
    })
    .await;
    assert_eq!(
        m.gauge(OUTBOX_OLDEST_AGE_SECONDS, &role),
        Some(OUTBOX_SAMPLE_S as f64)
    );
}

#[tokio::test]
async fn cancel_of_a_waiting_order_counts_without_a_duration() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("j2", req(512, 1, 600))).await;
    hub.accepted().await;
    hub.send(&cancel("j2", 2)).await;
    let m = h.meter.clone();
    let cancelled = [(RESULT, "cancelled"), (PLATFORM, "linux")];
    eventually("cancel counted", || {
        m.counter(ORDERS_TOTAL, &cancelled) == 1
    })
    .await;
    assert!(m.observed(JOB_DURATION_SECONDS, &cancelled).is_empty());
    eventually("queue empty", || m.gauge(QUEUE_LENGTH, LINUX) == Some(0.0)).await;
}

async fn started_notice(hub: &mut Conn) {
    loop {
        if let BrokerToHub::Started(_) = hub.hub().await {
            break;
        }
    }
}

#[tokio::test]
async fn waits_count_on_the_broker_clock_whatever_the_hub_clock_says() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j0", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order_at("late", req(512, 1, 600), START - 3_600_000))
        .await;
    hub.accepted().await;
    hub.send(&order_at("early", req(512, 1, 600), START + 3_600_000))
        .await;
    hub.accepted().await;
    tick_by(&h, 5).await;
    let m = h.meter.clone();
    let busy = [(REASON, "runner_busy")];
    eventually("saturation from acceptance", || {
        m.gauge(SATURATION_WAIT_SECONDS, &busy) == Some(5.0)
    })
    .await;
    r.send(&result("j0", 1, JobState::Passed, 0)).await;
    r.assign().await;
    assert_eq!(m.observed(QUEUE_WAIT_SECONDS, LINUX), vec![0.0, 5.0]);
}

#[tokio::test]
async fn a_requeued_order_waits_from_its_return_to_the_queue() {
    let h = Harness::start("");
    let a = h.member_runner("rA", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    let m = h.meter.clone();
    eventually("first assignment", || {
        m.observed(QUEUE_WAIT_SECONDS, LINUX).len() == 1
    })
    .await;
    h.clock.advance(Duration::from_secs(4));
    a.close().await;
    eventually("requeued", || m.gauge(QUEUE_LENGTH, LINUX) == Some(1.0)).await;
    h.clock.advance(Duration::from_secs(6));
    let mut b = h.member_runner("rB", "10.0.0.8", linux()).await;
    b.assign().await;
    assert_eq!(m.observed(QUEUE_WAIT_SECONDS, LINUX), vec![0.0, 6.0]);
}

#[tokio::test]
async fn same_machine_orders_given_back_count_as_cancelled() {
    let h = Harness::start("");
    let mut r = h.local_runner("alex", "lr", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("s1", same_machine_req())).await;
    hub.accepted().await;
    r.assign().await;
    hub.send(&order("s2", same_machine_req())).await;
    hub.accepted().await;
    r.close().await;
    let m = h.meter.clone();
    let cancelled = [(RESULT, "cancelled"), (PLATFORM, SAME_MACHINE)];
    eventually("both given back", || {
        m.counter(ORDERS_TOTAL, &cancelled) == 2
    })
    .await;
    assert!(m.observed(JOB_DURATION_SECONDS, &cancelled).is_empty());
    assert_clean(&m, &["s1", "s2", "alex", "local:alex"]);
}

#[tokio::test]
async fn timeout_and_lost_results_are_counted_with_their_duration() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    started_notice(&mut hub).await;
    h.clock.advance(Duration::from_secs(2));
    r.send(&result("j1", 2, JobState::Timeout, -1)).await;
    let m = h.meter.clone();
    let timeout = [(RESULT, "timeout"), (PLATFORM, "linux")];
    eventually("timeout counted", || m.counter(ORDERS_TOTAL, &timeout) == 1).await;
    assert_eq!(m.observed(JOB_DURATION_SECONDS, &timeout), vec![2.0]);
    hub.send(&order("j2", req(512, 1, 600))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j2", 1)).await;
    started_notice(&mut hub).await;
    h.clock.advance(Duration::from_secs(3));
    h.handle.revoked("r1").await;
    let lost = [(RESULT, "lost"), (PLATFORM, "linux")];
    eventually("lost counted", || m.counter(ORDERS_TOTAL, &lost) == 1).await;
    assert_eq!(m.observed(JOB_DURATION_SECONDS, &lost), vec![3.0]);
}
