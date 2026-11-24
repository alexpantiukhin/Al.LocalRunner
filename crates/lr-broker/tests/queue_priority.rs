mod common;

use common::harness::*;
use lr_core::JobId;
use lr_proto::*;

const CI_FIRST: &str =
    "[[rule]]\nname = \"ci-first\"\nwhen = { client_label = \"ci\" }\npriority = 50\n";
const RELEASE: &str =
    "[[rule]]\nname = \"release-boost\"\nwhen = { client_label = \"ci\" }\npriority = 50\n";

fn status(reply: ReadReply) -> StatusView {
    match reply {
        ReadReply::Status(s) => *s,
        other => panic!("expected status, got {other:?}"),
    }
}

fn explain(reply: ReadReply) -> String {
    match reply {
        ReadReply::Explain { text, .. } => text,
        other => panic!("expected explain, got {other:?}"),
    }
}

fn explain_of(job: &str, client: Option<&str>) -> ReadRequest {
    ReadRequest::Explain {
        job: JobId(job.into()),
        client: client.map(str::to_string),
    }
}

async fn busy_pool(h: &Harness) -> Conn {
    let mut r = h.member_runner("r1", "10.0.0.7", linux()).await;
    let mut alex = h.local_hub("alex").await;
    alex.send(&order("j0", req(512, 1, 60))).await;
    alex.accepted().await;
    r.assign().await;
    r
}

#[tokio::test]
async fn the_queue_view_shows_the_priority_in_the_order_the_scheduler_takes() {
    let h = Harness::start(&format!(
        "{CI_FIRST}[visibility]\nothers_orders = \"full\"\n"
    ));
    let _r = busy_pool(&h).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    alex.send(&order("a1", req(512, 1, 60))).await;
    alex.accepted().await;
    ci.send(&order("c1", req(512, 1, 60))).await;
    ci.accepted().await;
    let view = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    let queue: Vec<(&str, u32, Option<i64>)> = view
        .queue
        .iter()
        .map(|q| (q.summary.job.0.as_str(), q.position, q.priority))
        .collect();
    assert_eq!(
        queue,
        [("c1", 1, Some(50)), ("a1", 2, Some(0))],
        "the later order of higher priority goes first, and both say why"
    );
}

#[tokio::test]
async fn profile_labels_are_separate_from_the_client_label_and_explain_names_the_rule() {
    let h = Harness::start(&format!(
        "{RELEASE}[visibility]\nothers_orders = \"full\"\n"
    ));
    let _r = busy_pool(&h).await;
    let mut alex = h.local_hub("alex").await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    ci.send(&order_labelled(
        "c1",
        req(512, 1, 60),
        &["release", "nightly"],
    ))
    .await;
    ci.accepted().await;
    alex.send(&order("c2", req(512, 1, 60))).await;
    alex.accepted().await;
    let view = status(
        alex.read(ReadRequest::Status(StatusFilter::default()))
            .await,
    );
    let first = &view.queue[0];
    assert_eq!(first.summary.job, JobId("c1".into()));
    assert_eq!(first.summary.labels, ["release", "nightly"]);
    assert_eq!(
        first.summary.label.as_deref(),
        Some("agent-1"),
        "the client label (--as) is its own field"
    );
    assert_eq!(first.priority, Some(50));
    let second = &view.queue[1];
    assert!(second.summary.labels.is_empty());
    assert_eq!(second.priority, Some(0));

    let text = explain(alex.read(explain_of("c1", None)).await);
    assert!(text.contains("profile labels: release, nightly"), "{text}");
    assert!(
        text.contains("priority: 50 from rule release-boost (when ")
            && text.contains("client_label"),
        "{text}"
    );
    let text = explain(alex.read(explain_of("c2", None)).await);
    assert!(
        text.contains("priority: no rule sets a priority: 0"),
        "{text}"
    );
}

#[tokio::test]
async fn a_viewer_who_may_not_see_the_fields_of_an_order_gets_neither_priority_nor_labels() {
    let h = Harness::start(CI_FIRST);
    let _r = busy_pool(&h).await;
    let mut ci = h.member_hub("ci-box", "ci", "10.0.0.20").await;
    ci.send(&order_labelled("c1", req(512, 1, 60), &["release"]))
        .await;
    ci.accepted().await;
    let own = status(ci.read(ReadRequest::Status(StatusFilter::default())).await);
    assert_eq!(own.queue[0].priority, Some(50));
    assert_eq!(own.queue[0].summary.labels, ["release"]);

    let mut bob = h.local_hub("bob").await;
    let theirs = status(bob.read(ReadRequest::Status(StatusFilter::default())).await);
    let seen = theirs
        .queue
        .iter()
        .find(|q| q.summary.job == JobId("c1".into()))
        .expect("the order is listed at summary visibility");
    assert_eq!(seen.priority, None);
    assert!(seen.summary.labels.is_empty());
    assert_eq!(seen.summary.label, None);
    let json = serde_json::to_value(seen).unwrap();
    assert!(json.get("priority").is_none(), "{json}");

    let text = explain(bob.read(explain_of("c1", None)).await);
    assert!(!text.contains("priority"), "{text}");
}
