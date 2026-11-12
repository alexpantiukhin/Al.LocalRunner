mod common;

use common::harness::*;
use common::{err, ok, HOST};
use lr_broker::policy::{check, ResourceMode};
use lr_core::{JobId, JobState};
use lr_proto::*;

fn history(reply: ReadReply) -> Vec<JobSummary> {
    match reply {
        ReadReply::History { jobs, .. } => jobs,
        other => panic!("expected history, got {other:?}"),
    }
}

fn exceeded(job: &str, seq: u64) -> RunnerToBroker {
    RunnerToBroker::JobResult(JobResult {
        job: JobId(job.into()),
        seq,
        state: JobState::Failed,
        exit_code: None,
        reason: Some(ReasonCode::ResourceExceeded),
        detail: Some("превышен заявленный mem_mb: заявлено 512 МБ, пик 900 МБ".into()),
        peak_mem_mb: Some(900),
        peak_disk_mb: Some(40),
    })
}

#[test]
fn without_the_section_there_are_no_resources() {
    let f = ok("");
    assert!(f.resources.is_none());
    assert!(check(&f, &HOST).is_empty());
}

#[test]
fn the_section_is_read_with_its_defaults() {
    let f = ok("[resources]\n");
    let r = f.resources.expect("section present");
    assert_eq!(r.mode, ResourceMode::Warn);
    assert_eq!(r.mem_headroom.percent(), 125);
    assert_eq!(r.disk_headroom.percent(), 125);
    let w = r.wire();
    assert_eq!(w.mode, WireEnforce::Warn);
    assert_eq!((w.mem_headroom_pct, w.disk_headroom_pct), (125, 125));
}

#[test]
fn modes_and_headrooms_are_read() {
    let f = ok("[resources]\nmode = \"kill\"\nmem_headroom = 1.5\ndisk_headroom = 2\n").resources;
    let w = f.expect("section present").wire();
    assert_eq!(w.mode, WireEnforce::Kill);
    assert_eq!((w.mem_headroom_pct, w.disk_headroom_pct), (150, 200));
    let w = ok("[resources]\nmode = \"warn\"\nmem_headroom = 1.1\n")
        .resources
        .expect("section present")
        .wire();
    assert_eq!(w.mode, WireEnforce::Warn);
    assert_eq!(w.mem_headroom_pct, 110);
}

#[test]
fn invalid_values_are_refused_with_table_key_and_line() {
    let e = err("[resources]\nmode = \"stop\"\n");
    assert_eq!(e.table.as_deref(), Some("resources"));
    assert_eq!(e.key.as_deref(), Some("mode"));
    assert_eq!(e.line, Some(2));
    let e = err("[resources]\nmem_headroom = 0.9\n");
    assert_eq!(e.key.as_deref(), Some("mem_headroom"));
    assert_eq!(e.line, Some(2));
    let e = err("[resources]\ndisk_headroom = \"big\"\n");
    assert_eq!(e.key.as_deref(), Some("disk_headroom"));
    let e = err("[resources]\nmem_headroom = 1000\n");
    assert_eq!(e.key.as_deref(), Some("mem_headroom"));
    let e = err("[resources]\ncpu_headroom = 1.5\n");
    assert_eq!(e.key.as_deref(), Some("cpu_headroom"));
}

#[test]
fn kill_mode_is_not_a_warning_but_a_headroom_without_effect_is() {
    assert!(check(&ok("[resources]\nmode = \"kill\"\n"), &HOST).is_empty());
    assert!(check(&ok("[resources]\nmode = \"off\"\n"), &HOST).is_empty());
    let w = check(&ok("[resources]\nmode = \"off\"\nmem_headroom = 2.0\n"), &HOST);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].table.as_deref(), Some("resources"));
    assert_eq!(w[0].key.as_deref(), Some("mem_headroom"));
}

#[tokio::test]
async fn no_limits_are_sent_without_the_section() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    assert_eq!(r.assign().await.spec.limits, None);
}

#[tokio::test]
async fn limits_are_sent_when_the_section_exists_even_in_off_mode() {
    let h = Harness::start("[resources]\nmode = \"off\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    let limits = r.assign().await.spec.limits.expect("limits present");
    assert_eq!(limits.mode, WireEnforce::Off);
    assert_eq!(limits.mem_headroom_pct, 125);
}

#[tokio::test]
async fn kill_mode_limits_travel_with_the_assignment() {
    let h = Harness::start("[resources]\nmode = \"kill\"\nmem_headroom = 1.4\ndisk_headroom = 3\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    let limits = r.assign().await.spec.limits.expect("limits present");
    assert_eq!(
        limits,
        WireLimits {
            mode: WireEnforce::Kill,
            mem_headroom_pct: 140,
            disk_headroom_pct: 300,
        }
    );
}

#[tokio::test]
async fn an_exceeded_result_is_stored_failed_with_reason_text_and_peaks() {
    let h = Harness::start("[resources]\nmode = \"kill\"\n");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("j1", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("j1", 1)).await;
    r.send(&exceeded("j1", 2)).await;
    let rec = h.until_state("local:alex", "j1", JobState::Failed).await;
    assert_eq!(rec.job.peak_mem_mb, Some(900));
    assert_eq!(rec.job.peak_disk_mb, Some(40));
    let stored = rec.job.cancel_reason.expect("reason stored");
    assert!(
        stored.starts_with("resource_exceeded: ") && stored.contains("пик 900 МБ"),
        "{stored}"
    );
    let jobs = history(
        hub.read(ReadRequest::History(HistoryFilter::default()))
            .await,
    );
    let j = jobs.iter().find(|j| j.job.0 == "j1").expect("in history");
    assert_eq!(j.state, JobState::Failed);
    assert_eq!(j.reason, Some(ReasonCode::ResourceExceeded));
    assert_eq!((j.peak_mem_mb, j.peak_disk_mb), (Some(900), Some(40)));
    let detail = hub
        .read(ReadRequest::HistoryShow {
            job: JobId("j1".into()),
            client: None,
        })
        .await;
    match detail {
        ReadReply::Job(d) => assert_eq!(d.summary.peak_mem_mb, Some(900)),
        other => panic!("expected a job, got {other:?}"),
    }
    let explain = hub
        .read(ReadRequest::Explain {
            job: JobId("j1".into()),
            client: None,
        })
        .await;
    match explain {
        ReadReply::Explain { text, .. } => {
            assert!(text.contains("resource_exceeded"), "{text}");
            assert!(text.contains("пик памяти 900 МБ"), "{text}");
            assert!(text.contains("пик диска 40 МБ"), "{text}");
        }
        other => panic!("expected an explanation, got {other:?}"),
    }
}

#[tokio::test]
async fn peaks_of_a_passed_job_are_kept_and_a_job_without_them_has_none() {
    let h = Harness::start("");
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut hub = h.local_hub("alex").await;
    hub.send(&order("a", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("a", 1)).await;
    let mut with = result("a", 2, JobState::Passed, 0);
    if let RunnerToBroker::JobResult(x) = &mut with {
        x.peak_mem_mb = Some(300);
        x.peak_disk_mb = Some(12);
    }
    r.send(&with).await;
    h.until_state("local:alex", "a", JobState::Passed).await;
    hub.send(&order("b", req(512, 1, 60))).await;
    hub.accepted().await;
    r.assign().await;
    r.send(&started("b", 1)).await;
    r.send(&result("b", 2, JobState::Passed, 0)).await;
    h.until_state("local:alex", "b", JobState::Passed).await;
    let a = h.job_row("local:alex", "a").await.expect("a stored").job;
    assert_eq!((a.peak_mem_mb, a.peak_disk_mb), (Some(300), Some(12)));
    let b = h.job_row("local:alex", "b").await.expect("b stored").job;
    assert_eq!((b.peak_mem_mb, b.peak_disk_mb), (None, None));
}

#[tokio::test]
async fn enforcement_of_a_runner_is_shown_and_survives_a_restart() {
    let h = Harness::start("");
    let mut c = caps(&["linux"], 16384, 8);
    c.enforcement = Some("watcher".into());
    let _r = h.member_runner("r1", "10.0.0.7", c).await;
    let mut hub = h.local_hub("alex").await;
    let runners = |reply: ReadReply| match reply {
        ReadReply::Runners { runners } => runners,
        other => panic!("expected runners, got {other:?}"),
    };
    let list = runners(hub.read(ReadRequest::Runners).await);
    assert_eq!(list[0].caps.enforcement.as_deref(), Some("watcher"));
    let h2 = h.restart().await;
    let mut hub2 = h2.local_hub("alex").await;
    let list = runners(hub2.read(ReadRequest::Runners).await);
    assert_eq!(list[0].caps.enforcement.as_deref(), Some("watcher"));
    assert!(!list[0].online);
}
