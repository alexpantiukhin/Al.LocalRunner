mod common;

use common::harness::*;
use common::{err, ok, HOST};
use lr_broker::policy::{check, rule_policy, ForeignLoad};
use lr_core::{Admission, ForeignLoadMode};
use lr_proto::*;

const WAIT_MODE: &str = "[admission]\nforeign_load = \"wait\"\nmem_headroom_mb = 0\n";

fn load(free_mem_mb: u64, cpu_busy_percent: u8) -> RunnerToBroker {
    RunnerToBroker::Heartbeat {
        load: Some(WireLoad {
            free_mem_mb,
            cpu_busy_percent,
            cpu_cores: 8,
        }),
    }
}

fn status(reply: ReadReply) -> StatusView {
    match reply {
        ReadReply::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn without_the_section_the_load_is_ignored() {
    let f = ok("");
    assert_eq!(f.admission.foreign_load, ForeignLoad::Off);
    assert_eq!(f.admission.mem_headroom_mb, 256);
    assert_eq!(rule_policy(&f, &HOST).admission, Admission::default());
    assert_eq!(Admission::default().foreign_load, ForeignLoadMode::Off);
    assert!(check(&f, &HOST).is_empty());
}

#[test]
fn the_section_is_read() {
    let f = ok("[admission]\nforeign_load = \"wait\"\nmem_headroom_mb = 512\n");
    let a = rule_policy(&f, &HOST).admission;
    assert_eq!(a.foreign_load, ForeignLoadMode::Wait);
    assert_eq!(a.mem_headroom_mb, 512);
}

#[test]
fn invalid_values_are_refused_with_table_key_and_line() {
    let e = err("[admission]\nforeign_load = \"pause\"\n");
    assert_eq!(e.table.as_deref(), Some("admission"));
    assert_eq!(e.key.as_deref(), Some("foreign_load"));
    assert_eq!(e.line, Some(2));
    let e = err("[admission]\nmem_headroom_mb = -1\n");
    assert_eq!(e.key.as_deref(), Some("mem_headroom_mb"));
    let e = err("[admission]\nforeign_cpu = 1\n");
    assert_eq!(e.key.as_deref(), Some("foreign_cpu"));
}

#[test]
fn a_headroom_that_blocks_every_order_is_warned_about() {
    let f = ok("[admission]\nforeign_load = \"wait\"\nmem_headroom_mb = 40000\n");
    let w = check(&f, &HOST);
    assert!(
        w.iter()
            .any(|w| w.key.as_deref() == Some("mem_headroom_mb")),
        "{w:?}"
    );
    let f = ok("[admission]\nmem_headroom_mb = 1024\n");
    let w = check(&f, &HOST);
    assert!(w.iter().any(|w| w.message.contains("no effect")), "{w:?}");
}

#[tokio::test]
async fn an_order_waits_for_free_memory_and_starts_when_it_appears() {
    let h = Harness::start(WAIT_MODE);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&load(500, 0)).await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    r.quiet().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        s.queue[0].wait_reason.as_deref(),
        Some("занято чужими процессами: свободно 500 МБ, нужно 1024")
    );
    assert_eq!(s.queue[0].wait_code.as_deref(), Some("foreign_load"));
    r.send(&load(4096, 0)).await;
    r.assign().await;
}

#[tokio::test]
async fn foreign_cpu_keeps_the_order_queued() {
    let h = Harness::start(WAIT_MODE);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&load(8192, 95)).await;
    alex.send(&order("j1", req(1024, 4, 60))).await;
    alex.accepted().await;
    r.quiet().await;
    let s = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    assert_eq!(
        s.queue[0].wait_reason.as_deref(),
        Some("занято чужими процессами: CPU 95%, нужно 4 ядер")
    );
    r.send(&load(8192, 10)).await;
    r.assign().await;
}

#[tokio::test]
async fn a_runner_that_sends_no_load_is_admitted() {
    let h = Harness::start(WAIT_MODE);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
}

#[tokio::test]
async fn by_default_the_load_does_not_hold_an_order() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&load(1, 100)).await;
    alex.send(&order("j1", req(1024, 4, 60))).await;
    alex.accepted().await;
    r.assign().await;
}

#[tokio::test]
async fn a_running_job_is_not_touched_by_a_rising_load() {
    let h = Harness::start(WAIT_MODE);
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    r.send(&load(8192, 0)).await;
    alex.send(&order("j1", req(1024, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&load(10, 100)).await;
    r.acked(1).await;
    r.quiet().await;
}
