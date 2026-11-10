mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::http::{dashboard, host, login, send};
use lr_abstractions::CancelRequest;
use lr_core::JobId;
use lr_dashboard::{CSRF_HEADER, LOG_TAIL_BYTES};
use serde_json::Value;

fn post(uri: &str, cookie: &str, csrf: Option<&str>, body: String) -> Request<Body> {
    let mut req = Request::post(uri)
        .header(header::HOST, host())
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(csrf) = csrf {
        req = req.header(CSRF_HEADER, csrf);
    }
    req.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn cancel_many_needs_token_bounds_list_and_reports_each_job() {
    let source = common::source();
    let dash = dashboard(source.clone());
    let router = dash.router(common::http::PORT);
    let (cookie, csrf) = login(&router, dash.launch_token()).await;
    let body = |jobs: Vec<String>| serde_json::json!({"jobs": jobs, "reason": "bulk"}).to_string();
    let two = vec!["j-run".to_string(), "j-queued".to_string()];

    let (status, _, _) = send(
        &router,
        post("/api/cancel-many", &cookie, None, body(two.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &router,
        post("/api/cancel-many", &cookie, Some(&csrf), body(vec![])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let too_many = (0..501).map(|i| format!("j{i}")).collect();
    let (status, _, _) = send(
        &router,
        post("/api/cancel-many", &cookie, Some(&csrf), body(too_many)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(source.cancels().is_empty());

    let (status, _, json) = send(
        &router,
        post("/api/cancel-many", &cookie, Some(&csrf), body(two.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let outcomes: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(outcomes[0]["job"], "j-run");
    assert_eq!(outcomes[0]["ok"], true);
    assert_eq!(outcomes[1]["ok"], true);
    let actors: Vec<_> = source
        .cancels()
        .into_iter()
        .map(|(job, r)| (job.0, r.actor))
        .collect();
    assert_eq!(
        actors,
        [
            ("j-run".to_string(), "dashboard:alex".to_string()),
            ("j-queued".to_string(), "dashboard:alex".to_string())
        ]
    );

    source.set_unavailable(Some("brokers down".into()));
    let (status, _, json) = send(
        &router,
        post("/api/cancel-many", &cookie, Some(&csrf), body(two)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let outcomes: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(outcomes[0]["ok"], false);
    assert!(outcomes[1]["error"]
        .as_str()
        .unwrap()
        .contains("brokers down"));
}

#[tokio::test]
async fn cancel_reason_loses_control_characters() {
    let source = common::source();
    let dash = dashboard(source.clone());
    let router = dash.router(common::http::PORT);
    let (cookie, csrf) = login(&router, dash.launch_token()).await;
    let body = serde_json::json!({"job": "j-run", "reason": "stuck\n[fake] passed\r\u{1b}[31m"})
        .to_string();
    let (status, _, _) = send(&router, post("/api/cancel", &cookie, Some(&csrf), body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        source.cancels(),
        [(
            JobId("j-run".into()),
            CancelRequest {
                actor: "dashboard:alex".into(),
                reason: Some("stuck [fake] passed  [31m".into()),
            }
        )]
    );
}

#[tokio::test]
async fn log_answer_is_a_bounded_tail() {
    let source = common::source();
    let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    let log = format!(
        "FIRST LINE\n{}LAST LINE\n",
        line.repeat(LOG_TAIL_BYTES / line.len() + 10)
    );
    source.set_log(JobId("j-run".into()), log.clone().into_bytes());
    let dash = dashboard(source.clone());
    let router = dash.router(common::http::PORT);
    let (cookie, _) = login(&router, dash.launch_token()).await;
    let req = Request::get("/api/log/j-run")
        .header(header::HOST, host())
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let (status, headers, text) = send(&router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.len() <= LOG_TAIL_BYTES + 100, "{}", text.len());
    assert!(text.starts_with("[… "), "{}", &text[..60]);
    assert!(text.ends_with("LAST LINE\n"));
    assert!(!text.contains("FIRST LINE"));
    let cut: usize = headers["x-lr-log-cut"].to_str().unwrap().parse().unwrap();
    assert!(cut > 0);
    assert_eq!(&text[text.find('\n').unwrap() + 1..], &log[cut..]);
}
