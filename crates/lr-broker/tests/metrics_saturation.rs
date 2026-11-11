mod common;

use common::harness::*;
use lr_abstractions::metric::*;
use lr_core::Requirements;
use std::time::Duration;

const REASONS: [&str; 5] = ["runner_busy", "memory", "cpu", "tag", "rule"];

async fn tick_by(h: &Harness, secs: u64) {
    for _ in 0..secs {
        h.clock.advance(Duration::from_secs(1));
        tokio::task::yield_now().await;
    }
}

async fn saturation_after(h: &Harness, secs: u64, expected: Option<&str>) {
    tick_by(h, secs).await;
    let want = |reason: &str| {
        if Some(reason) == expected {
            Some(secs as f64)
        } else {
            Some(0.0)
        }
    };
    for _ in 0..500 {
        if REASONS
            .iter()
            .all(|r| h.meter.gauge(SATURATION_WAIT_SECONDS, &[(REASON, r)]) == want(r))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got: Vec<(&str, Option<f64>)> = REASONS
        .iter()
        .map(|r| (*r, h.meter.gauge(SATURATION_WAIT_SECONDS, &[(REASON, r)])))
        .collect();
    panic!("expected only {expected:?} at {secs} s, got {got:?}");
}

async fn first_runs_second_waits(h: &Harness, first: Requirements, second: Requirements) {
    let mut gpu = linux();
    gpu.tags.insert("db".into());
    let mut r1 = h.member_runner("r1", "10.0.0.7", gpu.clone()).await;
    let _r2 = h.member_runner("r2", "10.0.0.8", gpu).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", first)).await;
    hub.accepted().await;
    r1.assign().await;
    hub.send(&order("j2", second)).await;
    hub.accepted().await;
}

#[tokio::test]
async fn an_order_waiting_for_pool_memory_saturates_memory() {
    let h = Harness::start("");
    first_runs_second_waits(&h, req(12_000, 1, 600), req(8_000, 1, 600)).await;
    saturation_after(&h, 20, Some("memory")).await;
}

#[tokio::test]
async fn an_order_waiting_for_pool_cpu_saturates_cpu() {
    let h = Harness::start("");
    first_runs_second_waits(&h, req(512, 6, 600), req(512, 4, 600)).await;
    saturation_after(&h, 15, Some("cpu")).await;
}

#[tokio::test]
async fn an_order_waiting_for_an_exclusive_tag_saturates_tag() {
    let h = Harness::start("");
    let mut first = req(512, 1, 600);
    first.exclusive.insert("db".into());
    let mut second = req(512, 1, 600);
    second.exclusive.insert("db".into());
    first_runs_second_waits(&h, first, second).await;
    saturation_after(&h, 12, Some("tag")).await;
}

#[tokio::test]
async fn an_order_held_by_a_reserve_saturates_rule() {
    let h = Harness::start(
        "[[rule]]\nname = \"ci-reserve\"\nwhen = { client_label = \"ci\" }\n\
         reserve = { cpu = 6, mem_mb = 1024 }\n",
    );
    first_runs_second_waits(&h, req(512, 1, 600), req(512, 2, 600)).await;
    saturation_after(&h, 9, Some("rule")).await;
}

#[tokio::test]
async fn an_order_waiting_for_an_offline_runner_is_not_saturation() {
    let h = Harness::start("");
    let r = h.member_runner("r1", "10.0.0.7", linux()).await;
    r.close().await;
    let mut hub = h.local_hub("alex").await;
    hub.until_runner("r1", false).await;
    hub.send(&order("j1", req(512, 1, 600))).await;
    hub.accepted().await;
    saturation_after(&h, 30, None).await;
}
