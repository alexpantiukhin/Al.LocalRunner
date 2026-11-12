mod common;

use common::*;
use lr_proto::*;
use serde_json::{json, Value};

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn a_summary_of_an_older_peer_parses_and_empty_new_fields_are_not_sent() {
    let s = summary();
    let v = serde_json::to_value(&s).unwrap();
    for new in ["timeout_s", "github_run_url"] {
        assert!(
            !keys(&v).contains(&new.to_string()),
            "{new} not sent when empty"
        );
    }
    let old: JobSummary = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(old.timeout_s, None);
    assert_eq!(old.github_run_url, None);

    let mut newer = v;
    newer["timeout_s"] = json!(600);
    newer["github_run_url"] = json!("https://github.com/o/r/actions/runs/1");
    let parsed: JobSummary = serde_json::from_value(newer).unwrap();
    assert_eq!(parsed.timeout_s, Some(600));
    assert_eq!(
        parsed.github_run_url.as_deref(),
        Some("https://github.com/o/r/actions/runs/1")
    );
}

#[test]
fn a_queue_item_and_a_dashboard_job_of_an_older_peer_parse_without_the_wait_code() {
    let item = json!({"summary": serde_json::to_value(summary()).unwrap(), "position": 1, "wait_reason": "busy"});
    let parsed: QueueItem = serde_json::from_value(item).unwrap();
    assert_eq!(parsed.wait_code, None);
    let v = serde_json::to_value(&parsed).unwrap();
    assert!(!keys(&v).contains(&"wait_code".to_string()));

    let detail = json!({
        "summary": serde_json::to_value(summary()).unwrap(), "note": null, "branch": null,
        "cwd": null, "commit": null, "os_user": null, "source": null, "github_run_url": null,
        "steps": [], "secrets": [], "cancelled_by": null, "wait_reason": null
    });
    let job = json!({"broker": "home", "list": "queued", "detail": detail});
    let parsed: DashboardJob = serde_json::from_value(job).unwrap();
    assert_eq!(parsed.wait_code, None);
    let v = serde_json::to_value(&parsed).unwrap();
    assert!(!keys(&v).contains(&"wait_code".to_string()));
}

#[test]
fn the_history_details_request_and_reply_round_trip() {
    let req = ReadRequest::HistoryDetails(HistoryFilter::default());
    let text = serde_json::to_string(&req).unwrap();
    assert_eq!(serde_json::from_str::<ReadRequest>(&text).unwrap(), req);
    let reply = ReadReply::Details {
        jobs: Vec::new(),
        next: Some("c1".into()),
    };
    let text = serde_json::to_string(&reply).unwrap();
    assert_eq!(serde_json::from_str::<ReadReply>(&text).unwrap(), reply);
}
